//! Windows Credential Manager backend.
//!
//! Maps `(account, key)` onto a generic-type credential with target name
//! `"<account>/<key>"`. We use the wide-string (`*W`) variants so non-ASCII
//! account names and values round-trip cleanly.

#![cfg(target_os = "windows")]

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::slice;

use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredEnumerateW, CredFree, CredReadW, CredWriteW, CREDENTIALW,
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

use crate::secret_store::Backend;

pub(super) struct WinCredStore;

impl WinCredStore {
    pub(super) fn new() -> Self {
        WinCredStore
    }
}

impl Backend for WinCredStore {
    fn name(&self) -> &'static str {
        "wincred"
    }

    fn get(&self, account: &str, key: &str) -> io::Result<Option<String>> {
        let target = target_name(account, key);
        let target_w = to_utf16_z(&target);
        let mut out: *mut CREDENTIALW = std::ptr::null_mut();
        let ok = unsafe { CredReadW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0, &mut out) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_NOT_FOUND {
                return Ok(None);
            }
            return Err(io::Error::other(format!("CredReadW failed: code {err}")));
        }
        let value = unsafe {
            let cred = &*out;
            let len = cred.CredentialBlobSize as usize;
            let bytes = if cred.CredentialBlob.is_null() || len == 0 {
                Vec::new()
            } else {
                slice::from_raw_parts(cred.CredentialBlob, len).to_vec()
            };
            CredFree(out as *const core::ffi::c_void);
            bytes
        };
        let value = String::from_utf8(value)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        Ok(Some(value))
    }

    fn set(&self, account: &str, key: &str, value: &str) -> io::Result<()> {
        let target = target_name(account, key);
        let mut target_w = to_utf16_z(&target);
        let mut username_w = to_utf16_z(key);
        let blob = value.as_bytes();

        let mut cred: CREDENTIALW = unsafe { core::mem::zeroed() };
        cred.Type = CRED_TYPE_GENERIC;
        cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
        cred.TargetName = target_w.as_mut_ptr();
        cred.UserName = username_w.as_mut_ptr();
        cred.CredentialBlobSize = blob.len() as u32;
        cred.CredentialBlob = blob.as_ptr() as *mut u8;

        let ok = unsafe { CredWriteW(&cred, 0) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(io::Error::other(format!("CredWriteW failed: code {err}")));
        }
        Ok(())
    }

    fn delete(&self, account: &str, key: &str) -> io::Result<bool> {
        let target = target_name(account, key);
        let target_w = to_utf16_z(&target);
        let ok = unsafe { CredDeleteW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok != 0 {
            return Ok(true);
        }
        let err = unsafe { GetLastError() };
        if err == ERROR_NOT_FOUND {
            Ok(false)
        } else {
            Err(io::Error::other(format!("CredDeleteW failed: code {err}")))
        }
    }

    fn list(&self, account: &str) -> io::Result<Vec<String>> {
        let prefix = format!("{account}/");
        let filter = to_utf16_z(&format!("{prefix}*"));
        let mut count: u32 = 0;
        let mut creds: *mut *mut CREDENTIALW = std::ptr::null_mut();
        let ok = unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut creds) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_NOT_FOUND {
                return Ok(Vec::new());
            }
            return Err(io::Error::other(format!(
                "CredEnumerateW failed: code {err}"
            )));
        }
        let mut keys = Vec::with_capacity(count as usize);
        unsafe {
            let entries = slice::from_raw_parts(creds, count as usize);
            for entry in entries {
                let cred = &**entry;
                if !cred.TargetName.is_null() {
                    let name = utf16_z_to_string(cred.TargetName);
                    if let Some(rest) = name.strip_prefix(&prefix) {
                        keys.push(rest.to_string());
                    }
                }
            }
            CredFree(creds as *const core::ffi::c_void);
        }
        keys.sort();
        Ok(keys)
    }
}

fn target_name(account: &str, key: &str) -> String {
    format!("{account}/{key}")
}

fn to_utf16_z(s: &str) -> Vec<u16> {
    std::ffi::OsString::from(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Read a NUL-terminated UTF-16 wide string from a Windows credentials
/// pointer. Safe so long as the pointer originates from the Credentials
/// API (always NUL-terminated when non-null).
unsafe fn utf16_z_to_string(ptr: *const u16) -> String {
    let mut len = 0;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let slice = unsafe { slice::from_raw_parts(ptr, len) };
    String::from_utf16_lossy(slice)
}
