use std::io;

use harn_vm::secrets::NativeKeyring;

use crate::secret_store::Backend;

pub(super) struct NativeStore;

impl NativeStore {
    pub(super) fn new() -> Self {
        Self
    }

    fn keyring(account: &str) -> NativeKeyring {
        NativeKeyring::new(account)
    }
}

impl Backend for NativeStore {
    fn name(&self) -> &'static str {
        "keyring"
    }

    fn get(&self, account: &str, key: &str) -> io::Result<Option<String>> {
        Self::keyring(account)
            .get_string(key)
            .map_err(io::Error::other)
    }

    fn set(&self, account: &str, key: &str, value: &str) -> io::Result<()> {
        Self::keyring(account)
            .set_string(key, value)
            .map_err(io::Error::other)
    }

    fn delete(&self, account: &str, key: &str) -> io::Result<bool> {
        Self::keyring(account).delete(key).map_err(io::Error::other)
    }

    fn list(&self, account: &str) -> io::Result<Vec<String>> {
        Self::keyring(account).list().map_err(io::Error::other)
    }
}
