//! Identity of the loaded image containing Harn, including a macOS dylib.

#[cfg(not(target_vendor = "apple"))]
pub(super) fn linked_id() -> Option<String> {
    buildid::build_id()
        .filter(|id| !id.is_empty())
        .map(hex::encode)
}

#[cfg(target_vendor = "apple")]
// Keep dyld's native header layouts with its libc lookup instead of defining
// another Mach-O representation in the runtime.
#[allow(deprecated)]
pub(super) fn linked_id() -> Option<String> {
    let mut image = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    // SAFETY: dladdr initializes image on success. The address belongs to this
    // runtime, so dyld supplies its loaded executable or dylib's Mach-O header.
    let found = unsafe {
        libc::dladdr(
            (super::runtime_content_fingerprint as *const ()).cast::<libc::c_void>(),
            image.as_mut_ptr(),
        )
    };
    if found == 0 {
        return None;
    }
    // SAFETY: the successful lookup initialized every Dl_info field.
    let image = unsafe { image.assume_init() };
    if image.dli_fbase.is_null() {
        return None;
    }
    // SAFETY: dyld's image base points to a loaded native Mach-O header. Its
    // load-command region remains mapped while this runtime is executing.
    let header = unsafe { &*image.dli_fbase.cast::<libc::mach_header>() };
    let header_size = match header.magic {
        libc::MH_MAGIC => std::mem::size_of::<libc::mach_header>(),
        libc::MH_MAGIC_64 => std::mem::size_of::<libc::mach_header_64>(),
        _ => return None,
    };
    let length = usize::try_from(header.sizeofcmds).ok()?;
    if length > isize::MAX as usize {
        return None;
    }
    // SAFETY: sizeofcmds describes the loaded header's contiguous command
    // region; the parser checks each command against that region's bounds.
    let commands = unsafe {
        std::slice::from_raw_parts(image.dli_fbase.cast::<u8>().add(header_size), length)
    };
    mach_uuid(commands, header.ncmds)
}

#[cfg(any(target_vendor = "apple", test))]
fn mach_uuid(mut commands: &[u8], count: u32) -> Option<String> {
    const LC_UUID: u32 = 0x1b;
    let mut uuid = None;
    for _ in 0..count {
        let command = u32::from_ne_bytes(commands.get(..4)?.try_into().ok()?);
        let size = u32::from_ne_bytes(commands.get(4..8)?.try_into().ok()?) as usize;
        if size < 8 || size > commands.len() {
            return None;
        }
        if command == LC_UUID {
            if size != 24 || uuid.is_some() {
                return None;
            }
            let bytes = commands.get(8..24)?;
            if bytes.iter().all(|byte| *byte == 0) {
                return None;
            }
            uuid = Some(hex::encode(bytes));
        }
        commands = &commands[size..];
    }
    commands.is_empty().then_some(uuid).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mach_identity_requires_one_complete_nonempty_uuid() {
        let command = [0x1bu32.to_ne_bytes(), 24u32.to_ne_bytes()].concat();
        let valid = [command.as_slice(), &[7u8; 16]].concat();
        assert_eq!(mach_uuid(&valid, 1), Some("07".repeat(16)));
        assert_eq!(mach_uuid(&valid, 0), None);
        assert_eq!(mach_uuid(&valid, 2), None);
        assert_eq!(mach_uuid(&valid[..23], 1), None);
        assert_eq!(mach_uuid(&[valid.as_slice(), &valid].concat(), 2), None);
        assert_eq!(
            mach_uuid(&[command.as_slice(), &[0u8; 16]].concat(), 1),
            None
        );
        assert_eq!(mach_uuid(&[0; 8], 1), None);
    }
}
