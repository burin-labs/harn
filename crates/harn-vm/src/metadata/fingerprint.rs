use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Clone, Copy)]
enum Kind {
    Structure,
    Content,
}

pub(super) fn compute_structure_hash(dir: &Path) -> io::Result<String> {
    fingerprint(dir, Kind::Structure)
}

pub(super) fn compute_content_hash_for_dir(dir: &Path) -> io::Result<String> {
    fingerprint(dir, Kind::Content)
}

fn fingerprint(dir: &Path, kind: Kind) -> io::Result<String> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        entries.push((entry.file_name(), entry.metadata()?));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hash = Fingerprint(0xcbf29ce484222325);
    hash.write(b"harn-directory-v2");
    for (name, metadata) in entries {
        let name = name.as_encoded_bytes();
        hash.write(&(name.len() as u64).to_le_bytes());
        hash.write(name);
        hash.write(&[u8::from(metadata.is_dir()), u8::from(metadata.is_symlink())]);
        hash.write(&metadata.len().to_le_bytes());
        if matches!(kind, Kind::Content) {
            let modified = metadata.modified()?;
            let (before_epoch, duration) = match modified.duration_since(UNIX_EPOCH) {
                Ok(duration) => (false, duration),
                Err(error) => (true, error.duration()),
            };
            hash.write(&[u8::from(before_epoch)]);
            hash.write(&duration.as_secs().to_le_bytes());
            hash.write(&duration.subsec_nanos().to_le_bytes());
        }
    }
    Ok(format!("{:016x}", hash.0))
}

// FNV-1a is sufficient for a local staleness fingerprint, not authentication.
struct Fingerprint(u64);

impl Fingerprint {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

#[cfg(test)]
mod tests;
