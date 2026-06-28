//! Apple Keychain-backed secret store. The `account` namespace maps to
//! `kSecAttrService`, the `key` argument to `kSecAttrAccount`, matching the
//! Swift `KeychainCredentialStore` layout from an IDE host so existing
//! credentials are reachable without migration.

#![cfg(any(target_os = "macos", target_os = "ios"))]

use std::io;

use security_framework::item::{ItemClass, ItemSearchOptions, Limit, SearchResult};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

use crate::secret_store::Backend;

/// `errSecItemNotFound`. Returned when a Keychain query has no match.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

pub(super) struct KeychainStore;

impl KeychainStore {
    pub(super) fn new() -> Self {
        KeychainStore
    }
}

impl Backend for KeychainStore {
    fn name(&self) -> &'static str {
        "keychain"
    }

    fn get(&self, account: &str, key: &str) -> io::Result<Option<String>> {
        match get_generic_password(account, key) {
            Ok(bytes) => {
                let value = String::from_utf8(bytes)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                Ok(Some(value))
            }
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(err) => Err(io::Error::other(format!("keychain get: {err}"))),
        }
    }

    fn set(&self, account: &str, key: &str, value: &str) -> io::Result<()> {
        set_generic_password(account, key, value.as_bytes())
            .map_err(|err| io::Error::other(format!("keychain set: {err}")))
    }

    fn delete(&self, account: &str, key: &str) -> io::Result<bool> {
        match delete_generic_password(account, key) {
            Ok(()) => Ok(true),
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(false),
            Err(err) => Err(io::Error::other(format!("keychain delete: {err}"))),
        }
    }

    fn list(&self, account: &str) -> io::Result<Vec<String>> {
        let mut search = ItemSearchOptions::new();
        search
            .class(ItemClass::generic_password())
            .service(account)
            .limit(Limit::All)
            .load_attributes(true);

        let results = match search.search() {
            Ok(results) => results,
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => return Ok(Vec::new()),
            Err(err) => return Err(io::Error::other(format!("keychain list: {err}"))),
        };

        let mut keys = Vec::with_capacity(results.len());
        for entry in results {
            if let SearchResult::Dict(_) = &entry {
                if let Some(attrs) = entry.simplify_dict() {
                    if let Some(name) = attrs.get("acct") {
                        keys.push(name.clone());
                    }
                }
            }
        }
        keys.sort();
        Ok(keys)
    }
}
