//! Explicit private-file adapter for the existing secret-provider contract.
//!
//! Storage is a flat JSON map of percent-encoded identifiers to base64 values.
//! Writers share `<path>.lock.sqlite3`, DELETE journal mode and BEGIN IMMEDIATE
//! with the existing Swift host writer. Atomic replacement alone cannot protect
//! a read-modify-write transaction from a concurrent writer.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use zeroize::{Zeroize, Zeroizing};

use super::{
    emit_secret_access_event, ensure_scoped_secret_access_allowed, RotationHandle, SecretBytes,
    SecretDeleteRequest, SecretError, SecretId, SecretMeta, SecretProvider, SecretVersion,
};

pub const SECRET_FILE_PATH_ENV: &str = "HARN_SECRET_FILE_PATH";
const PROVIDER: &str = "file";
const LOCK_WAIT: Duration = Duration::from_secs(3);

/// Durable secrets at an explicitly selected private path.
///
/// Unix permissions and ownership are checked before reading or replacing a
/// store. Platforms without this permission adapter refuse construction rather
/// than treating an unenforced mode as private storage. No default path or
/// automatic fallback from another provider is selected here.
#[derive(Clone, Debug)]
pub struct FileSecretProvider {
    path: PathBuf,
}

impl FileSecretProvider {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, SecretError> {
        if !cfg!(unix) {
            return Err(SecretError::Unsupported {
                provider: PROVIDER.into(),
                operation: "private_file_storage",
            });
        }
        let path = path.into();
        if !path.is_absolute()
            || path.file_name().is_none()
            || path.components().any(|part| part == Component::ParentDir)
        {
            return Err(SecretError::InvalidConfig(format!(
                "{SECRET_FILE_PATH_ENV} must name an absolute file without parent traversal"
            )));
        }
        Ok(Self { path })
    }

    fn transaction<T>(
        &self,
        write: bool,
        operation: impl FnOnce(&mut BTreeMap<String, String>) -> Result<T, SecretError>,
    ) -> Result<T, SecretError> {
        let parent = self.path.parent().expect("validated file path has parent");
        prepare_directory(parent)?;
        let mut lock_name = self.path.as_os_str().to_owned();
        lock_name.push(".lock.sqlite3");
        let lock_path = PathBuf::from(lock_name);
        let lock_file = open_private(&lock_path, true)?;
        // Retain the checked handle while SQLite owns its transaction. The
        // private directory prevents another user from replacing this path.
        let mut connection = Connection::open_with_flags(
            &lock_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| backend("cannot open the secret transaction lock"))?;
        connection
            .busy_timeout(LOCK_WAIT)
            .and_then(|()| connection.execute_batch("PRAGMA journal_mode=DELETE"))
            .map_err(|_| backend("cannot configure the secret transaction lock"))?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| backend("secret transaction lock unavailable within 3 seconds"))?;
        let mut contents = self.load()?;
        let result = operation(&mut contents.0)?;
        if write {
            let bytes = Zeroizing::new(
                serde_json::to_vec(&contents.0)
                    .map_err(|_| backend("cannot encode the secret store"))?,
            );
            crate::atomic_io::atomic_write_with_mode(&self.path, &bytes, 0o600)
                .map_err(|_| backend("cannot atomically persist the secret store"))?;
        }
        transaction
            .commit()
            .map_err(|_| backend("cannot release the secret transaction lock"))?;
        drop(lock_file);
        Ok(result)
    }

    fn load(&self) -> Result<Contents, SecretError> {
        let mut file = match open_private(&self.path, false) {
            Ok(file) => file,
            Err(OpenError::Absent) => return Ok(Contents::default()),
            Err(OpenError::Secret(error)) => return Err(error),
        };
        let mut bytes = Zeroizing::new(Vec::new());
        file.read_to_end(&mut bytes)
            .map_err(|_| backend("cannot read the secret store"))?;
        serde_json::from_slice(&bytes)
            .map(Contents)
            .map_err(|_| backend("secret store must be a JSON object of encoded string values"))
    }
}

#[derive(Default)]
struct Contents(BTreeMap<String, String>);

impl Drop for Contents {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            value.zeroize();
        }
    }
}

enum OpenError {
    Absent,
    Secret(SecretError),
}

impl From<OpenError> for SecretError {
    fn from(error: OpenError) -> Self {
        match error {
            OpenError::Absent => backend("secret transaction lock disappeared"),
            OpenError::Secret(error) => error,
        }
    }
}

fn backend(message: &str) -> SecretError {
    SecretError::Backend {
        provider: PROVIDER.into(),
        message: message.into(),
    }
}

fn prepare_directory(path: &Path) -> Result<(), SecretError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|_| backend("cannot create the private secret directory"))?;
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| backend("cannot inspect the private secret directory"))?;
    if !metadata.is_dir() || !owner_only(&metadata) {
        return Err(backend(
            "secret directory must be owned by this user with mode 0700",
        ));
    }
    Ok(())
}

fn owner_only(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // The low six mode bits are the group/other permissions.
        // SAFETY: geteuid has no arguments or memory-safety preconditions.
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode().trailing_zeros() >= 6
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

fn open_private(path: &Path, create: bool) -> Result<File, OpenError> {
    let mut options = OpenOptions::new();
    options.read(true).write(create).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| {
        if !create && error.kind() == std::io::ErrorKind::NotFound {
            OpenError::Absent
        } else {
            OpenError::Secret(backend("cannot open the private secret file"))
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|_| OpenError::Secret(backend("cannot inspect the private secret file")))?;
    if !metadata.is_file() || !owner_only(&metadata) {
        return Err(OpenError::Secret(backend(
            "secret file must be regular, owned by this user, and owner-only (chmod 600)",
        )));
    }
    Ok(file)
}

#[async_trait]
impl SecretProvider for FileSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        let value = self.transaction(false, |all| {
            let value = all
                .get(&storage_key(id))
                .ok_or_else(|| SecretError::NotFound {
                    provider: PROVIDER.into(),
                    id: id.clone(),
                })?;
            base64::engine::general_purpose::STANDARD
                .decode(value)
                .map(SecretBytes::from)
                .map_err(|_| backend("stored secret is not valid base64"))
        })?;
        emit_secret_access_event(PROVIDER, id);
        Ok(value)
    }

    async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError> {
        self.transaction(true, |all| {
            let encoded =
                value.with_exposed(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes));
            if let Some(mut old) = all.insert(storage_key(id), encoded) {
                old.zeroize();
            }
            Ok(())
        })
    }

    async fn delete_scoped(&self, request: SecretDeleteRequest) -> Result<(), SecretError> {
        ensure_scoped_secret_access_allowed("delete", &request.id)?;
        self.transaction(true, |all| {
            if let Some(mut old) = all.remove(&storage_key(&request.id)) {
                old.zeroize();
            }
            Ok(())
        })
    }

    async fn list(&self, prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        self.transaction(false, |all| {
            Ok(all
                .keys()
                .filter_map(|key| decode_storage_key(key))
                .filter(|id| {
                    id.namespace == prefix.namespace
                        && id.name.starts_with(&prefix.name)
                        && (prefix.version == SecretVersion::Latest || id.version == prefix.version)
                })
                .map(|id| SecretMeta {
                    id,
                    provider: PROVIDER.into(),
                })
                .collect())
        })
    }

    async fn rotate(&self, _id: &SecretId) -> Result<RotationHandle, SecretError> {
        Err(SecretError::Unsupported {
            provider: PROVIDER.into(),
            operation: "rotate",
        })
    }

    fn namespace(&self) -> &str {
        PROVIDER
    }
    fn supports_versions(&self) -> bool {
        false
    }
}

fn encode_component(value: &str, allow_slash: bool) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) || (allow_slash && byte == b'/')
        {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(&mut encoded, "%{byte:02X}").expect("writing a String cannot fail");
        }
    }
    encoded
}

fn storage_key(id: &SecretId) -> String {
    let mut key = format!(
        "{}/{}",
        encode_component(&id.namespace, false),
        encode_component(&id.name, true)
    );
    if let SecretVersion::Exact(version) = id.version {
        key.push_str(&format!("#v{version}"));
    }
    key
}

fn decode_component(value: &str) -> Option<String> {
    let mut decoded = Vec::new();
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).ok()
}

fn decode_storage_key(value: &str) -> Option<SecretId> {
    let (namespace, account) = value.split_once('/')?;
    let (name, version) = account
        .rsplit_once("#v")
        .and_then(|(name, version)| {
            version
                .parse()
                .ok()
                .map(|version| (name, SecretVersion::Exact(version)))
        })
        .unwrap_or((account, SecretVersion::Latest));
    Some(SecretId::new(decode_component(namespace)?, decode_component(name)?).with_version(version))
}

#[cfg(all(test, unix))]
mod tests;
