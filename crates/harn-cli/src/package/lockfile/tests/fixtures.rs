//! Shared fixtures for the lock-file tests.

use crate::package::*;

pub(super) fn write_tar_gz_package_archive(root: &Path, archive_path: &Path) {
    fn append_files(
        builder: &mut tar::Builder<flate2::write::GzEncoder<File>>,
        root: &Path,
        cursor: &Path,
    ) {
        let mut entries = fs::read_dir(cursor)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                append_files(builder, root, &path);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                builder.append_path_with_name(&path, relative).unwrap();
            }
        }
    }

    let file = File::create(archive_path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_files(&mut builder, root, root);
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap();
}
