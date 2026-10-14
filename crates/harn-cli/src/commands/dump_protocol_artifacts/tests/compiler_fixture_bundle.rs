use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BUNDLE_DIRECTORY: &str = "open-enum-compiler";

#[derive(Deserialize, Serialize)]
struct Asset {
    filename: String,
    sha256: String,
}

#[derive(Deserialize, Serialize)]
struct Bundle {
    schema: String,
    test_binary_sha256: String,
    roots: BTreeMap<String, String>,
    assets: Vec<Asset>,
}

fn digest(path: &Path) -> String {
    use std::io::Read;
    let mut file = std::fs::File::open(path).expect("compiler asset exists");
    let mut hash = Sha256::new();
    let mut buffer = [0; 16_384];
    loop {
        let count = file.read(&mut buffer).expect("read compiler asset");
        if count == 0 {
            break;
        }
        hash.update(buffer.get(..count).expect("bounded read"));
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fingerprint(path: &Path) -> u64 {
    let encoded = std::fs::read_to_string(path).expect("Cargo library fingerprint");
    let encoded = encoded.trim();
    assert_eq!(encoded.len(), 16, "Cargo library fingerprint width");
    let mut bytes = [0; 8];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    u64::from_le_bytes(bytes)
}

fn metadata(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).expect("actual compilation metadata"))
        .expect("typed Cargo compilation metadata")
}

struct Compilation {
    executable: PathBuf,
    test_metadata: serde_json::Value,
    libraries: BTreeMap<u64, Vec<(PathBuf, PathBuf)>>,
}

impl Compilation {
    fn read() -> Self {
        let executable = std::env::current_exe().expect("test binary");
        let dependencies = executable
            .parent()
            .expect("dependency directory")
            .to_owned();
        let fingerprints = dependencies.parent().unwrap().join(".fingerprint");
        let identity = executable
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .trim_end_matches(std::env::consts::EXE_SUFFIX);
        let test_metadata = metadata(
            &fingerprints
                .join(identity.replace("harn_cli-", "harn-cli-"))
                .join("test-lib-harn_cli.json"),
        );
        let mut libraries: BTreeMap<u64, Vec<(PathBuf, PathBuf)>> = BTreeMap::new();
        for entry in std::fs::read_dir(&fingerprints).expect("current target fingerprints") {
            let entry = entry.unwrap();
            let identity = entry.file_name().to_string_lossy().into_owned();
            let Some((_, hash)) = identity.rsplit_once('-') else {
                continue;
            };
            for file in std::fs::read_dir(entry.path()).unwrap() {
                let file = file.unwrap();
                let filename = file.file_name().to_string_lossy().into_owned();
                let Some(name) = filename.strip_prefix("lib-") else {
                    continue;
                };
                if file.path().extension().is_some() {
                    continue;
                }
                let candidates = [
                    dependencies.join(format!("lib{name}-{hash}.rmeta")),
                    dependencies.join(format!("lib{name}-{hash}.dylib")),
                    dependencies.join(format!("lib{name}-{hash}.so")),
                    dependencies.join(format!("{name}-{hash}.dll")),
                ];
                if let Some(artifact) = candidates.into_iter().find(|path| path.is_file()) {
                    libraries
                        .entry(fingerprint(&file.path()))
                        .or_default()
                        .push((file.path(), artifact));
                }
            }
        }
        Self {
            executable,
            test_metadata,
            libraries,
        }
    }

    fn root(&self, name: &str) -> u64 {
        let matches = self.test_metadata["deps"]
            .as_array()
            .expect("measured test dependencies")
            .iter()
            .filter(|dependency| dependency[1].as_str() == Some(name))
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "exact test dependency identity for {name}"
        );
        matches[0][3].as_u64().expect("dependency fingerprint")
    }

    fn library(&self, identity: u64) -> &(PathBuf, PathBuf) {
        let matches = self
            .libraries
            .get(&identity)
            .expect("bound compiler dependency exists");
        assert_eq!(
            matches.len(),
            1,
            "one exact compiler artifact per Cargo identity"
        );
        &matches[0]
    }
}

fn bundle_directory() -> PathBuf {
    let executable = std::env::current_exe().expect("test binary");
    let debug = executable.parent().unwrap().parent().unwrap();
    assert_eq!(
        debug.file_name().unwrap(),
        "debug",
        "native dev archive contract"
    );
    debug.parent().unwrap().join(BUNDLE_DIRECTORY)
}

fn actual_compilation() -> &'static Compilation {
    static COMPILATION: std::sync::OnceLock<Compilation> = std::sync::OnceLock::new();
    COMPILATION.get_or_init(Compilation::read)
}

fn archived_bundle() -> &'static Bundle {
    static BUNDLE: std::sync::OnceLock<Bundle> = std::sync::OnceLock::new();
    BUNDLE.get_or_init(|| {
        let directory = bundle_directory();
        let bundle: Bundle = serde_json::from_slice(
            &std::fs::read(directory.join("manifest.json"))
                .expect("compiler archive fixture manifest"),
        )
        .expect("typed compiler fixture manifest");
        assert_eq!(bundle.schema, "harn.enum_compiler_fixture.v1");
        assert_eq!(
            bundle.test_binary_sha256,
            digest(&std::env::current_exe().unwrap()),
            "compiler fixture belongs to this exact test binary"
        );
        assert_eq!(
            bundle.roots.keys().map(String::as_str).collect::<Vec<_>>(),
            ["harn_vm", "serde"]
        );
        let mut names = BTreeSet::new();
        for asset in &bundle.assets {
            assert_eq!(
                Path::new(&asset.filename)
                    .file_name()
                    .and_then(|n| n.to_str()),
                Some(asset.filename.as_str()),
                "compiler asset is a plain filename"
            );
            assert!(
                names.insert(asset.filename.as_str()),
                "unique compiler asset"
            );
            assert_eq!(
                asset.sha256,
                digest(&directory.join(&asset.filename)),
                "archived compiler dependency identity"
            );
        }
        assert!(!names.is_empty(), "nonempty measured compiler closure");
        for root in bundle.roots.values() {
            assert!(
                names.contains(root.as_str()),
                "root is in the validated compiler closure"
            );
        }
        bundle
    })
}

pub(super) fn dependency(name: &str) -> PathBuf {
    let executable = std::env::current_exe().unwrap();
    let fingerprints = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(".fingerprint");
    if fingerprints.is_dir() {
        let compilation = actual_compilation();
        return compilation.library(compilation.root(name)).1.clone();
    }
    bundle_directory().join(
        archived_bundle()
            .roots
            .get(name)
            .expect("actual compiler root"),
    )
}

pub(super) fn dependency_directory() -> PathBuf {
    let executable = std::env::current_exe().unwrap();
    let dependencies = executable.parent().unwrap();
    if dependencies.parent().unwrap().join(".fingerprint").is_dir() {
        dependencies.to_owned()
    } else {
        archived_bundle();
        bundle_directory()
    }
}

pub(super) fn stage_if_requested() {
    let Ok(request) = std::env::var("HARN_ENUM_COMPILER_ARCHIVE_STAGE") else {
        return;
    };
    assert_eq!(request, "1", "explicit archive producer staging request");
    let compilation = actual_compilation();
    let mut pending = vec![compilation.root("harn_vm"), compilation.root("serde")];
    let mut closure = BTreeMap::new();
    while let Some(identity) = pending.pop() {
        if closure.contains_key(&identity) {
            continue;
        }
        let (fp, artifact) = compilation.library(identity);
        closure.insert(identity, artifact.clone());
        // Proc macros load their already linked host library. Only Rust metadata
        // requires transitive compiler assets; build scripts are producer inputs.
        if artifact
            .extension()
            .is_some_and(|extension| extension == "rmeta")
        {
            let library_metadata = metadata(&fp.with_extension("json"));
            for dependency in library_metadata["deps"]
                .as_array()
                .expect("library dependencies")
            {
                if !dependency[1].as_str().unwrap().starts_with("build_script_") {
                    pending.push(dependency[3].as_u64().expect("library dependency identity"));
                }
            }
        }
    }
    let directory = bundle_directory();
    let staging = tempfile::tempdir_in(directory.parent().unwrap()).expect("own target staging");
    let mut assets = Vec::new();
    for artifact in closure.values() {
        let filename = artifact.file_name().unwrap().to_str().unwrap().to_owned();
        std::fs::copy(artifact, staging.path().join(&filename))
            .expect("stage exact compiler asset");
        assets.push(Asset {
            filename,
            sha256: digest(artifact),
        });
    }
    let roots = ["harn_vm", "serde"]
        .into_iter()
        .map(|name| {
            let artifact = &compilation.library(compilation.root(name)).1;
            (
                name.to_owned(),
                artifact.file_name().unwrap().to_str().unwrap().to_owned(),
            )
        })
        .collect();
    let bundle = Bundle {
        schema: "harn.enum_compiler_fixture.v1".to_owned(),
        test_binary_sha256: digest(&compilation.executable),
        roots,
        assets,
    };
    std::fs::write(
        staging.path().join("manifest.json"),
        serde_json::to_vec(&bundle).unwrap(),
    )
    .expect("write compiler fixture identity");
    if directory.exists() {
        std::fs::remove_dir_all(&directory).expect("replace own reproducible compiler fixture");
    }
    std::fs::rename(staging.path(), &directory).expect("publish complete compiler fixture");
}
