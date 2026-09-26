//! Where a confined child may write, and how that is granted once per policy.
//!
//! The child's token (see [`super::token`]) is write-restricted: a write
//! succeeds only where one of its restricting SIDs may write, and the only
//! one of those this backend grants anything to is the policy SID. So the
//! policy's writable roots are exactly the directories whose DACL carries an
//! inheritable Modify entry for that SID. Reads are the user's own and need
//! no grant.
//!
//! A grant is a recursive rewrite (inheritance is not retroactive, so an
//! entry placed on a directory reaches the files already inside it only by
//! rewriting them), which costs seconds on a real workspace. The SID is
//! derived from the grant plan rather than from the spawn, so the entry is
//! durable: the first spawn under a plan pays for the rewrite and every later
//! one, in this process or another, finds it in place with one
//! non-recursive read.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    GetLastError, LocalFree, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HANDLE, UNICODE_STRING,
};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, GetSecurityInfo, SetEntriesInAclW, SetNamedSecurityInfoW,
    SetSecurityInfo, EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
    SE_KERNEL_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    EqualSid, GetAce, GetSecurityDescriptorControl, ACCESS_ALLOWED_ACE, ACL, CONTAINER_INHERIT_ACE,
    DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE, NO_INHERITANCE, OBJECT_INHERIT_ACE,
    PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::Memory::{OpenFileMappingW, SECTION_ALL_ACCESS};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

use super::token::Sid;
use super::{path_to_wide, sandbox_trace, str_to_wide, OwnedHandle};

use crate::orchestration::CapabilityPolicy;
use crate::stdlib::sandbox::{
    policy_allows_workspace_write, process_sandbox_policy_write_roots, process_sandbox_roots,
};

/// `icacls`'s Modify: read, write, execute and delete.
const MODIFY: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;

pub(super) struct PolicyWriteGrants {
    /// How many recursive rewrites this spawn paid for. Zero once a policy's
    /// grants are in place, which is the point of a durable policy SID.
    pub(super) rewrites: usize,
    /// The roots those rewrites touched, so a repeat names what it repeated.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) rewritten: Vec<PathBuf>,
}

/// The directories a policy may write: its workspace roots and process write
/// roots when it may write the workspace, nothing otherwise.
pub(super) fn writable_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    if !policy_allows_workspace_write(policy) {
        return Vec::new();
    }
    process_sandbox_roots(policy)
        .into_iter()
        .chain(process_sandbox_policy_write_roots(policy))
        .collect()
}

impl PolicyWriteGrants {
    /// Grant `sid` Modify on every writable root of `policy`, plus `extra`
    /// (the child's own scratch directory, when it needs one). A workspace
    /// root that does not exist fails the spawn, since a child that cannot
    /// reach its own workspace is not a usable sandbox.
    pub(super) fn grant(
        label: &str,
        sid: &Sid,
        policy: &CapabilityPolicy,
        extra: Option<&Path>,
    ) -> io::Result<Self> {
        for root in process_sandbox_roots(policy) {
            if !root.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("sandbox workspace root '{}' does not exist", root.display()),
                ));
            }
        }
        let sddl = sid.to_sddl()?;
        let mut rewrites = 0;
        let mut rewritten = Vec::new();
        let mut roots = writable_roots(policy);
        roots.extend(extra.map(Path::to_path_buf));
        for root in roots {
            if !root.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("sandbox write root '{}' does not exist", root.display()),
                ));
            }
            if policy_holds(&root, sid, &sddl) {
                sandbox_trace(
                    label,
                    format!(
                        "write grant skipped path={} reason=already-granted",
                        root.display()
                    ),
                );
                continue;
            }
            sandbox_trace(label, format!("write grant begin path={}", root.display()));
            grant_modify(&root, sid)?;
            sandbox_trace(label, "write grant ok");
            remember_grant(&root, &sddl);
            rewrites += 1;
            rewritten.push(root);
        }
        Ok(Self {
            rewrites,
            rewritten,
        })
    }
}

/// The digest of a policy's grant plan, from which its SID and scratch
/// directory are named.
///
/// The same writable roots and write permission name the same SID, and a
/// policy that differs in either names another, so a read-only run never
/// inherits a writable run's grant. Only the plan is hashed, so the SID
/// reveals no path.
pub(super) fn policy_digest(policy: &CapabilityPolicy) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let write = policy_allows_workspace_write(policy);
    let mut plan: Vec<String> = process_sandbox_roots(policy)
        .into_iter()
        .map(|root| format!("workspace:{}", root.display()))
        .chain(
            process_sandbox_policy_write_roots(policy)
                .into_iter()
                .filter(|_| write)
                .map(|root| format!("write:{}", root.display())),
        )
        .collect();
    plan.sort();
    plan.dedup();
    let mut hasher = Sha256::new();
    hasher.update(format!("restricted-token;write={write};"));
    for entry in &plan {
        hasher.update(entry.as_bytes());
        hasher.update([0]);
    }
    hasher.finalize().into()
}

fn granted() -> &'static std::sync::Mutex<BTreeSet<(PathBuf, String)>> {
    static GRANTS: std::sync::OnceLock<std::sync::Mutex<BTreeSet<(PathBuf, String)>>> =
        std::sync::OnceLock::new();
    GRANTS.get_or_init(|| std::sync::Mutex::new(BTreeSet::new()))
}

/// Forget what this process has seen, so a test can prove the on-disk probe
/// by itself, as a fresh process would.
#[cfg(test)]
pub(super) fn forget_grants() {
    if let Ok(mut grants) = granted().lock() {
        grants.clear();
    }
}

fn remember_grant(root: &Path, sddl: &str) {
    if let Ok(mut grants) = granted().lock() {
        grants.insert((root.to_path_buf(), sddl.to_string()));
    }
}

/// Whether `root` already carries the policy SID's inheritable Modify entry.
///
/// A non-recursive read of the root's own DACL, milliseconds against the
/// rewrite it saves. The root's entry stands for the tree: the grant was
/// propagated when it was made, and files created since inherit it. Checked
/// in this process first, then on disk, where another process may have made
/// it.
fn policy_holds(root: &Path, sid: &Sid, sddl: &str) -> bool {
    let key = (root.to_path_buf(), sddl.to_string());
    if granted().lock().is_ok_and(|grants| grants.contains(&key)) {
        return true;
    }
    let held = SecurityInfo::read(root).is_ok_and(|info| info.grants_modify(sid));
    if held {
        remember_grant(root, sddl);
    }
    held
}

/// A path's security descriptor, freed on drop.
struct SecurityInfo {
    descriptor: *mut core::ffi::c_void,
    dacl: *mut ACL,
}

impl SecurityInfo {
    fn read_handle(handle: HANDLE) -> io::Result<Self> {
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(Self { descriptor, dacl })
    }

    fn read(path: &Path) -> io::Result<Self> {
        let wide = path_to_wide(path);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(Self { descriptor, dacl })
    }

    fn dacl_is_protected(&self) -> bool {
        let mut control = 0u16;
        let mut revision = 0u32;
        let read =
            unsafe { GetSecurityDescriptorControl(self.descriptor, &mut control, &mut revision) };
        read != 0 && control & SE_DACL_PROTECTED != 0
    }

    /// Whether an explicit, inheritable allow entry gives `sid` Modify.
    fn grants_modify(&self, sid: &Sid) -> bool {
        let inherits = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8;
        self.allows(sid, MODIFY, inherits)
    }

    /// Whether an explicit, effective allow entry gives `sid` every right in
    /// `mask`, carrying at least the `inherits` flags.
    fn allows(&self, sid: &Sid, mask: u32, inherits: u8) -> bool {
        if self.dacl.is_null() {
            return false;
        }
        let count = unsafe { (*self.dacl).AceCount };
        (0..u32::from(count)).any(|index| {
            let mut ace = std::ptr::null_mut();
            if unsafe { GetAce(self.dacl, index, &mut ace) } == 0 {
                return false;
            }
            let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
            let header = unsafe { (*ace).Header };
            u32::from(header.AceType) == ACCESS_ALLOWED_ACE_TYPE
                && header.AceFlags & inherits == inherits
                && header.AceFlags & INHERIT_ONLY_ACE as u8 == 0
                && unsafe { (*ace).Mask } & mask == mask
                && unsafe {
                    EqualSid(
                        std::ptr::addr_of_mut!((*ace).SidStart).cast(),
                        sid.as_psid(),
                    )
                } != 0
        })
    }
}

impl Drop for SecurityInfo {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

/// Add an inheritable Modify entry for `sid` to `root` and propagate it to
/// what is already inside. Through the Win32 API rather than `icacls`, which
/// refuses a SID that no account resolves to. A root whose DACL was detached
/// from its parent stays detached.
fn grant_modify(root: &Path, sid: &Sid) -> io::Result<()> {
    let current = SecurityInfo::read(root)?;
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: MODIFY,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.as_psid().cast(),
        },
    };
    let mut widened: *mut ACL = std::ptr::null_mut();
    let status = unsafe { SetEntriesInAclW(1, &entry, current.dacl, &mut widened) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let mut information = DACL_SECURITY_INFORMATION;
    if current.dacl_is_protected() {
        information |= PROTECTED_DACL_SECURITY_INFORMATION;
    }
    let wide = path_to_wide(root);
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            information,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            widened,
            std::ptr::null(),
        )
    };
    unsafe {
        LocalFree(widened.cast());
    }
    if status != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "could not grant the sandbox write access to '{}': {}",
                root.display(),
                io::Error::from_raw_os_error(status as i32)
            ),
        ));
    }
    Ok(())
}

/// What [`grant_msys_user_sections`] found.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum MsysSection {
    /// No MSYS program of this user runs in this session; the child creates
    /// the section itself.
    Absent,
    AlreadyGranted,
    Granted,
}

/// Let a confined child open the user's MSYS shared-memory section.
///
/// Every MSYS program (Git for Windows' `bash`, `grep`, `sh`) opens a
/// per-user section named `<user SID>.<version>` with full access, in the
/// MSYS runtime's own object directory (`<runtime>S<n>-<install key>`) under
/// the session's `BaseNamedObjects`, and dies with `CreateFileMapping ...,
/// Win32 error 5` when it cannot. The first MSYS program of the session
/// creates it with its token's default DACL, which names only the user and
/// SYSTEM, so under the write-restricted token the write half of that open
/// fails: none of the restricting SIDs is in the DACL. A confined child can
/// create the section itself when none exists (measured), so only an
/// existing one needs the grant. The section lives only as long as some MSYS
/// process holds it, and the entry added for the policy SID goes with it.
/// Version 1 is the one current MSYS runtimes use.
///
/// The runtime directory's name carries a hash of the runtime's install
/// path, so rather than recompute it this looks for the section in every
/// object directory of the session namespace.
pub(super) fn grant_msys_user_sections(sid: &Sid, user_sddl: &str) -> io::Result<MsysSection> {
    let (root, prefix) = session_namespace()?;
    let mut found = 0;
    let mut granted = 0;
    for directory in object_directories(&root)? {
        match grant_section(&format!("{prefix}\\{directory}\\{user_sddl}.1"), sid)? {
            None => {}
            Some(newly) => {
                found += 1;
                granted += usize::from(newly);
            }
        }
    }
    Ok(match (found, granted) {
        (0, _) => MsysSection::Absent,
        (_, 0) => MsysSection::AlreadyGranted,
        _ => MsysSection::Granted,
    })
}

/// Grant `sid` full access to the named section. `None` when it does not
/// exist, otherwise whether an entry had to be added.
fn grant_section(name: &str, sid: &Sid) -> io::Result<Option<bool>> {
    let wide = str_to_wide(name);
    let handle = unsafe { OpenFileMappingW(READ_CONTROL | WRITE_DAC, 0, wide.as_ptr()) };
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        if error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let handle = OwnedHandle::new(handle);
    let current = SecurityInfo::read_handle(handle.raw())?;
    if current.allows(sid, SECTION_ALL_ACCESS, 0) {
        return Ok(Some(false));
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: SECTION_ALL_ACCESS,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.as_psid().cast(),
        },
    };
    let mut widened: *mut ACL = std::ptr::null_mut();
    let status = unsafe { SetEntriesInAclW(1, &entry, current.dacl, &mut widened) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let status = unsafe {
        SetSecurityInfo(
            handle.raw(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            widened,
            std::ptr::null(),
        )
    };
    unsafe {
        LocalFree(widened.cast());
    }
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(Some(true))
}

/// This session's `BaseNamedObjects` directory, and the Win32 prefix that
/// names it.
fn session_namespace() -> io::Result<(String, &'static str)> {
    let mut session = 0u32;
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(if session == 0 {
        ("\\BaseNamedObjects".to_string(), "Global")
    } else {
        (format!("\\Sessions\\{session}\\BaseNamedObjects"), "Local")
    })
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *const UNICODE_STRING,
    attributes: u32,
    security_descriptor: *const core::ffi::c_void,
    security_quality_of_service: *const core::ffi::c_void,
}

#[repr(C)]
struct ObjectDirectoryInformation {
    name: UNICODE_STRING,
    type_name: UNICODE_STRING,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtOpenDirectoryObject(
        handle: *mut HANDLE,
        access: u32,
        attributes: *const ObjectAttributes,
    ) -> i32;
    fn NtQueryDirectoryObject(
        handle: HANDLE,
        buffer: *mut core::ffi::c_void,
        length: u32,
        return_single_entry: u8,
        restart_scan: u8,
        context: *mut u32,
        return_length: *mut u32,
    ) -> i32;
}

const DIRECTORY_QUERY: u32 = 0x0001;
const OBJ_CASE_INSENSITIVE: u32 = 0x0040;
const STATUS_MORE_ENTRIES: i32 = 0x0000_0105;

fn unicode_to_string(value: &UNICODE_STRING) -> String {
    if value.Buffer.is_null() {
        return String::new();
    }
    let chars = unsafe { std::slice::from_raw_parts(value.Buffer, usize::from(value.Length) / 2) };
    String::from_utf16_lossy(chars)
}

/// The names of the object directories directly under `root`.
fn object_directories(root: &str) -> io::Result<Vec<String>> {
    let mut wide: Vec<u16> = root.encode_utf16().collect();
    let name = UNICODE_STRING {
        Length: (wide.len() * 2) as u16,
        MaximumLength: (wide.len() * 2) as u16,
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: std::ptr::null_mut(),
        object_name: &name,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: std::ptr::null(),
        security_quality_of_service: std::ptr::null(),
    };
    let mut raw = std::ptr::null_mut();
    let status = unsafe { NtOpenDirectoryObject(&mut raw, DIRECTORY_QUERY, &attributes) };
    if status < 0 {
        return Err(io::Error::other(format!(
            "NtOpenDirectoryObject({root}) failed: {status:#x}"
        )));
    }
    let directory = OwnedHandle::new(raw);
    let mut buffer = vec![0u64; 8192];
    let mut context = 0u32;
    let mut names = Vec::new();
    let mut restart = 1u8;
    loop {
        let mut returned = 0u32;
        let status = unsafe {
            NtQueryDirectoryObject(
                directory.raw(),
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 8) as u32,
                0,
                restart,
                &mut context,
                &mut returned,
            )
        };
        restart = 0;
        if status < 0 {
            // STATUS_NO_MORE_ENTRIES is a warning (0x8000_001A), so negative.
            break;
        }
        let mut entry = buffer.as_ptr().cast::<ObjectDirectoryInformation>();
        loop {
            let info = unsafe { &*entry };
            if info.name.Buffer.is_null() {
                break;
            }
            if unicode_to_string(&info.type_name) == "Directory" {
                names.push(unicode_to_string(&info.name));
            }
            entry = unsafe { entry.add(1) };
        }
        if status != STATUS_MORE_ENTRIES {
            break;
        }
    }
    Ok(names)
}
