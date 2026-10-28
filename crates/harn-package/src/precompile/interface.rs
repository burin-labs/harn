//! Keep imported types and their package authority within one generation window.

use super::*;

/// `after_setup` lets the owning test publish at the setup/digest boundary.
/// Production supplies a no-op; no environment switch or public API is added.
pub(super) fn prepare_consistent_interface(
    source_path: &Path,
    source: &str,
    interface_dest: &Path,
    mut after_setup: impl FnMut(),
) -> Result<PrecompiledInterface, String> {
    // One retry covers setup publishing the generation the first walk lacked.
    // A concurrent publisher can invalidate the retry too; refuse rather than
    // attach authority from another generation or retry without a bound.
    for _ in 0..2 {
        let generation = current_package_generation(source_path);
        let mut interface = harn_vm::bytecode_cache::prepare_precompiled_interface(
            interface_dest,
            source_path,
            source,
        )
        .map_err(|error| format!("prepare {}: {error}", interface_dest.display()))?;
        let has_packages = !interface.package_import_aliases().is_empty();
        if has_packages {
            let built;
            let graph = match interface.module_graph() {
                Some(graph) => graph,
                None => {
                    built = harn_modules::build_with_source(source_path, source);
                    &built
                }
            };
            package::ensure_reachable_dependencies_materialized(source_path, graph)
                .map_err(|error| error.to_string())?;
            after_setup();
        }
        let aliases = package::declared_package_import_aliases(
            source_path,
            interface.package_import_aliases(),
        )
        .map_err(|error| error.to_string())?;
        let digest = package::reachable_dependency_lock_digest(source_path, &aliases)
            .map_err(|error| error.to_string())?;

        // The bracket includes digest acquisition: it may wait for an install
        // and observe a generation newer than both the first walk and setup.
        // Generation identities are unique; a later publication cannot turn a
        // changed pointer back into the one that this interface was walked on.
        if has_packages
            && (generation.is_none() || current_package_generation(source_path) != generation)
        {
            continue;
        }
        interface.set_package_authority(aliases, digest);
        return Ok(interface);
    }
    Err(format!(
        "package generation changed while preparing {} after 2 attempts; retry precompile",
        source_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_guard::ScopedEnvVar;
    use crate::package::test_support::TestWorkspace;
    use crate::test_support::harn_state_lock::lock_harn_state;

    struct Fixture {
        _temporary: tempfile::TempDir,
        project: PathBuf,
        dependency: PathBuf,
        entry: PathBuf,
        source: String,
        workspace: TestWorkspace,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let project = temporary.path().join("consumer");
            let dependency = temporary.path().join("dependency");
            std::fs::create_dir_all(project.join(".git")).unwrap();
            std::fs::create_dir(&dependency).unwrap();
            std::fs::write(
                project.join("harn.toml"),
                "[package]\nname = \"consumer\"\n[dependencies]\nacme = { path = \"../dependency\" }\n",
            )
            .unwrap();
            std::fs::write(
                dependency.join("harn.toml"),
                "[package]\nname = \"acme\"\nversion = \"0.1.0\"\n",
            )
            .unwrap();
            let source = "import \"acme/lib\"\npub fn result(value: any) -> string {\n  match value {\n    Choice.Ready -> { return \"ready\" }\n    _ -> { return \"other\" }\n  }\n}\n".to_string();
            let entry = project.join("main.harn");
            std::fs::write(&entry, &source).unwrap();
            let workspace = TestWorkspace::new(&project);
            let fixture = Self {
                _temporary: temporary,
                project,
                dependency,
                entry,
                source,
                workspace,
            };
            fixture.publish(false);
            fixture
        }

        fn publish(&self, numeric: bool) {
            // An imported type name changes the lowering context, unlike a
            // function return annotation, which the interface does not retain.
            // Changing the manifest also changes the path dependency's locked
            // authority; a source-only live edit might leave that digest equal.
            let version = if numeric { "0.2.0" } else { "0.1.0" };
            std::fs::write(
                self.dependency.join("harn.toml"),
                format!("[package]\nname = \"acme\"\nversion = \"{version}\"\n"),
            )
            .unwrap();
            let module = if numeric {
                "pub enum Choice { Ready }\npub type After = number\n"
            } else {
                "pub enum Choice { Ready }\npub type Before = string\n"
            };
            std::fs::write(self.dependency.join("lib.harn"), module).unwrap();
            assert_eq!(
                package::install_packages_in(self.workspace.env(), false, None, false).unwrap(),
                1
            );
        }

        fn cache_guard(&self) -> ScopedEnvVar {
            ScopedEnvVar::set(
                "HARN_CACHE_DIR",
                self.project.join(".bytecode-cache").to_str().unwrap(),
            )
        }

        fn prepare(&self, name: &str, after_setup: impl FnMut()) -> PrecompiledInterface {
            prepare_consistent_interface(
                &self.entry,
                &self.source,
                &self.project.join(name),
                after_setup,
            )
            .unwrap()
        }
    }

    #[test]
    fn publication_after_setup_cannot_stamp_an_old_interface_with_new_authority() {
        let _state = lock_harn_state();
        let fixture = Fixture::new();
        let _cache = fixture.cache_guard();
        let before = fixture.prepare("before.harnifc", || {});
        let generation = current_package_generation(&fixture.entry).expect("known generation");
        let digest = package::reachable_dependency_lock_digest(&fixture.entry, &["acme".into()])
            .unwrap()
            .expect("known nonempty package authority");
        let mut boundaries = 0;
        let candidate = fixture.prepare("candidate.harnifc", || {
            boundaries += 1;
            if boundaries == 1 {
                assert_eq!(
                    current_package_generation(&fixture.entry),
                    Some(generation.clone()),
                    "setup did not replace the generation; this is the later race"
                );
                fixture.publish(true);
                assert_ne!(
                    current_package_generation(&fixture.entry),
                    Some(generation.clone()),
                    "the callback must actually publish a replacement"
                );
                assert_ne!(
                    package::reachable_dependency_lock_digest(&fixture.entry, &["acme".into()])
                        .unwrap()
                        .expect("replacement package authority"),
                    digest,
                    "the replacement must change the authority the old interface would receive"
                );
            }
        });
        candidate.store().unwrap();
        let fresh = fixture.prepare("fresh.harnifc", || {});
        fresh.store().unwrap();
        assert_ne!(
            before.context(),
            fresh.context(),
            "different exports reached"
        );
        assert_eq!(candidate.context(), fresh.context(), "generation agrees");
        let candidate_bytes = std::fs::read(fixture.project.join("candidate.harnifc")).unwrap();
        assert!(!candidate_bytes.is_empty(), "the interface was emitted");
        assert_eq!(
            candidate_bytes,
            std::fs::read(fixture.project.join("fresh.harnifc")).unwrap(),
            "the entire interface payload, key and authority must match a fresh walk"
        );
        assert_eq!(boundaries, 2, "the changed generation forced one retry");
    }

    #[test]
    fn stable_generation_keeps_the_first_interface_and_reproducible_bytes() {
        let _state = lock_harn_state();
        let fixture = Fixture::new();
        let _cache = fixture.cache_guard();
        let mut boundaries = 0;
        let first = fixture.prepare("stable.harnifc", || boundaries += 1);
        first.store().unwrap();
        let bytes = std::fs::read(fixture.project.join("stable.harnifc")).unwrap();
        let second = fixture.prepare("stable.harnifc", || boundaries += 1);
        second.store().unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(
            bytes,
            std::fs::read(fixture.project.join("stable.harnifc")).unwrap()
        );
        assert_eq!(boundaries, 2, "each stable operation needs one attempt");
    }

    #[test]
    fn repeated_publication_refuses_before_emitting_an_interface() {
        let _state = lock_harn_state();
        let fixture = Fixture::new();
        let _cache = fixture.cache_guard();
        let destination = fixture.project.join("unstable.harnifc");
        let mut boundaries = 0;
        let result =
            prepare_consistent_interface(&fixture.entry, &fixture.source, &destination, || {
                boundaries += 1;
                fixture.publish(boundaries % 2 == 1);
            });
        let error = match result {
            Ok(_) => panic!("a continuously replaced generation was accepted"),
            Err(error) => error,
        };
        assert_eq!(boundaries, 2, "the operation terminates at its retry bound");
        assert!(error.contains("after 2 attempts"), "{error}");
        assert!(
            !destination.exists(),
            "refusal must not publish an interface"
        );
    }
}
