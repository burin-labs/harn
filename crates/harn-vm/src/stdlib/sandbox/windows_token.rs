//! The identity a confined child runs as: a write-restricted copy of this
//! process's own token.
//!
//! `CreateRestrictedToken(WRITE_RESTRICTED)` leaves every read check as the
//! user's own, and makes every write check pass a second test against the
//! restricting SIDs: Everyone, the logon session, and one SID per policy. A
//! write the user may make still fails unless one of those SIDs may make it
//! too, and the only place the policy SID may write is where
//! [`super::acl_grants`] granted it. Reads are not confined by this token.
//!
//! Two measured requirements shape the rest. Without its own default DACL
//! the child cannot open the objects it creates during start-up (the
//! restricting SIDs are not in the inherited default DACL), and every
//! program, `cmd` included, dies with `STATUS_DLL_INIT_FAILED`. And the
//! default DACL lives in the token's small dynamic area, so it has to be
//! sized exactly: a generously sized ACL is refused with
//! `ERROR_ALLOTTED_SPACE_EXCEEDED`.

use std::io;

use windows_sys::Win32::Foundation::{LocalFree, GENERIC_ALL, HANDLE};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows_sys::Win32::Security::{
    AddAccessAllowedAce, CreateRestrictedToken, CreateWellKnownSid, GetLengthSid,
    GetTokenInformation, InitializeAcl, SetTokenInformation, TokenDefaultDacl, TokenGroups,
    TokenUser, WinLocalSystemSid, WinWorldSid, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, PSID,
    SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY,
    TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_QUERY, TOKEN_USER, WRITE_RESTRICTED,
};
use windows_sys::Win32::System::SystemServices::SE_GROUP_LOGON_ID;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{str_to_wide, wide_ptr_to_string, OwnedHandle};

const SID_WORDS: usize = (SECURITY_MAX_SID_SIZE as usize).div_ceil(4);

/// A SID in owned, 4-byte-aligned storage, so a pointer to it stays valid
/// for as long as the value lives.
pub(super) struct Sid(Box<[u32; SID_WORDS]>);

impl Sid {
    fn empty() -> Self {
        Sid(Box::new([0u32; SID_WORDS]))
    }

    fn copy_from(psid: PSID) -> Self {
        let len = unsafe { GetLengthSid(psid) } as usize;
        assert!(len <= SECURITY_MAX_SID_SIZE as usize, "SID length {len}");
        let mut sid = Self::empty();
        unsafe {
            std::ptr::copy_nonoverlapping(psid.cast::<u8>(), sid.0.as_mut_ptr().cast::<u8>(), len);
        }
        sid
    }

    fn well_known(kind: i32) -> io::Result<Self> {
        let mut sid = Self::empty();
        let mut size = SECURITY_MAX_SID_SIZE;
        if unsafe {
            CreateWellKnownSid(
                kind,
                std::ptr::null_mut(),
                sid.0.as_mut_ptr().cast(),
                &mut size,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(sid)
    }

    /// The SID a policy's grants are made to, from the digest of its grant
    /// plan. Domain-shaped (`S-1-5-21-a-b-c-1000`) because
    /// `CreateRestrictedToken` refuses an AppContainer-shaped SID as a
    /// restricting SID (measured: `ERROR_INVALID_PARAMETER`). Deterministic,
    /// so every spawn under the same plan finds its grants already in place.
    pub(super) fn for_policy_digest(digest: &[u8]) -> io::Result<Self> {
        let part = |index: usize| {
            let bytes: [u8; 4] = digest[index * 4..index * 4 + 4]
                .try_into()
                .expect("a policy digest has at least 12 bytes");
            u32::from_le_bytes(bytes)
        };
        let text = format!("S-1-5-21-{}-{}-{}-1000", part(0), part(1), part(2));
        let wide = str_to_wide(&text);
        let mut raw: PSID = std::ptr::null_mut();
        if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let sid = Self::copy_from(raw);
        unsafe {
            LocalFree(raw.cast());
        }
        Ok(sid)
    }

    pub(super) fn as_psid(&self) -> PSID {
        self.0.as_ptr().cast_mut().cast()
    }

    fn len(&self) -> usize {
        unsafe { GetLengthSid(self.as_psid()) as usize }
    }

    pub(super) fn to_sddl(&self) -> io::Result<String> {
        let mut raw = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(self.as_psid(), &mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let text = wide_ptr_to_string(raw);
        unsafe {
            LocalFree(raw.cast());
        }
        Ok(text)
    }
}

/// A primary token for a child confined by `policy_sid`.
pub(super) fn write_restricted_token(policy_sid: &Sid) -> io::Result<OwnedHandle> {
    let base = process_token()?;
    let user = user_sid(base.raw())?;
    let logon = logon_sid(base.raw())?;
    let everyone = Sid::well_known(WinWorldSid)?;
    let system = Sid::well_known(WinLocalSystemSid)?;

    let mut restricting = vec![&everyone];
    restricting.extend(logon.as_ref());
    restricting.push(policy_sid);
    let restricting: Vec<SID_AND_ATTRIBUTES> = restricting
        .iter()
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.as_psid(),
            Attributes: 0,
        })
        .collect();
    let mut token = std::ptr::null_mut();
    if unsafe {
        CreateRestrictedToken(
            base.raw(),
            WRITE_RESTRICTED,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            restricting.len() as u32,
            restricting.as_ptr(),
            &mut token,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle::new_checked(token)?;

    let mut owners = vec![&user, &system];
    owners.extend(logon.as_ref());
    owners.push(policy_sid);
    set_default_dacl(token.raw(), &owners)?;
    Ok(token)
}

/// This process's user SID, in string form.
pub(super) fn current_user_sddl() -> io::Result<String> {
    let base = process_token()?;
    user_sid(base.raw())?.to_sddl()
}

fn process_token() -> io::Result<OwnedHandle> {
    let mut token = std::ptr::null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
            &mut token,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    OwnedHandle::new_checked(token)
}

/// `class` of `token`, in 8-byte-aligned storage.
fn token_information(token: HANDLE, class: i32) -> io::Result<Vec<u64>> {
    let mut needed = 0u32;
    unsafe {
        GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    if unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

fn user_sid(token: HANDLE) -> io::Result<Sid> {
    let buffer = token_information(token, TokenUser)?;
    let user = buffer.as_ptr().cast::<TOKEN_USER>();
    Ok(Sid::copy_from(unsafe { (*user).User.Sid }))
}

/// The logon session's SID, which the window station and desktop admit. A
/// token without one (rare: some service contexts) is restricted without it.
fn logon_sid(token: HANDLE) -> io::Result<Option<Sid>> {
    let buffer = token_information(token, TokenGroups)?;
    let groups = buffer.as_ptr().cast::<TOKEN_GROUPS>();
    let entries = unsafe {
        std::slice::from_raw_parts((*groups).Groups.as_ptr(), (*groups).GroupCount as usize)
    };
    let flag = SE_GROUP_LOGON_ID as u32;
    Ok(entries
        .iter()
        .find(|entry| entry.Attributes & flag == flag)
        .map(|entry| Sid::copy_from(entry.Sid)))
}

/// Replace the token's default DACL with one granting `owners` full access.
fn set_default_dacl(token: HANDLE, owners: &[&Sid]) -> io::Result<()> {
    let ace_header = std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<u32>();
    let bytes = owners
        .iter()
        .fold(std::mem::size_of::<ACL>(), |total, sid| {
            total + ace_header + sid.len()
        })
        .div_ceil(4)
        * 4;
    let mut storage = vec![0u32; bytes / 4];
    let acl = storage.as_mut_ptr().cast::<ACL>();
    if unsafe { InitializeAcl(acl, bytes as u32, ACL_REVISION) } == 0 {
        return Err(io::Error::last_os_error());
    }
    for sid in owners {
        if unsafe { AddAccessAllowedAce(acl, ACL_REVISION, GENERIC_ALL, sid.as_psid()) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let info = TOKEN_DEFAULT_DACL { DefaultDacl: acl };
    if unsafe {
        SetTokenInformation(
            token,
            TokenDefaultDacl,
            std::ptr::from_ref(&info).cast(),
            std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
