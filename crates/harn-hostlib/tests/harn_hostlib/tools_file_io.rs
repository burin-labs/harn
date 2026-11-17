//! Integration tests for `hostlib_tools_{read_file,write_file,delete_file,list_directory}`.

use std::fs;

use harn_hostlib::HostlibError;
use harn_vm::VmValue;
use tempfile::TempDir;

use super::support::*;

#[test]
fn read_file_returns_utf8_payload_with_size() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("hello.txt");
    fs::write(&file, "hello world").unwrap();

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_read_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[("path", vm_string(&path_str(&file)))])).unwrap();

    assert!(matches!(dict_get(&result, "encoding"), VmValue::String(s) if s.as_str() == "utf-8"));
    assert!(
        matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_str() == "hello world")
    );
    assert!(matches!(dict_get(&result, "size"), VmValue::Int(11)));
    assert!(matches!(
        dict_get(&result, "truncated"),
        VmValue::Bool(false)
    ));
}

#[test]
fn read_file_offset_and_limit_apply() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("payload.txt");
    fs::write(&file, "0123456789").unwrap();

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_read_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("offset", VmValue::Int(2)),
        ("limit_bytes", VmValue::Int(4)),
    ]))
    .unwrap();

    assert!(matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_str() == "2345"));
    assert!(matches!(
        dict_get(&result, "truncated"),
        VmValue::Bool(true)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn session_reads_keep_physical_offset_reads_bounded() {
    use std::io::{Seek, Write};

    fn characters_read() -> u64 {
        fs::read_to_string("/proc/self/io")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("rchar: "))
            .unwrap()
            .parse()
            .unwrap()
    }

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sparse");
    let length = 8 * 1024 * 1024;
    let mut file = fs::File::create(&path).unwrap();
    file.set_len(length).unwrap();
    file.seek(std::io::SeekFrom::Start(length - 4)).unwrap();
    file.write_all(b"END!").unwrap();
    let registry = tools_registry();
    let read = registry.find("hostlib_tools_read_file").unwrap();
    for mode in [
        harn_hostlib::fs::FsMode::Immediate,
        harn_hostlib::fs::FsMode::Staged,
    ] {
        let session = uuid::Uuid::new_v4().to_string();
        harn_hostlib::fs::set_mode(&session, mode, Some(dir.path())).unwrap();
        let before = characters_read();
        let result = (read.handler)(&dict_arg(&[
            ("path", vm_string(&path_str(&path))),
            ("session_id", vm_string(&session)),
            ("offset", VmValue::Int((length - 4) as i64)),
            ("limit_bytes", VmValue::Int(4)),
        ]))
        .unwrap();
        let read_characters = characters_read() - before;
        assert!(matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_str() == "END!"));
        assert!(matches!(
            dict_get(&result, "truncated"),
            VmValue::Bool(false)
        ));
        assert!(
            read_characters < 8192,
            "bounded read consumed {read_characters} bytes"
        );
    }
}

#[cfg(unix)]
#[test]
fn session_fifo_reads_refuse_without_blocking_other_session_reads() {
    use std::os::unix::fs::OpenOptionsExt;

    let dir = TempDir::new().unwrap();
    let fifo = dir.path().join("fifo");
    let c_path = std::ffi::CString::new(path_str(&fifo)).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    // A regressed reader can open the FIFO but cannot reach EOF until cleanup.
    let keeper = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    let session = uuid::Uuid::new_v4().to_string();
    harn_hostlib::fs::configure_session_root(&session, dir.path());
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let registry = registry_with(&[
            &harn_hostlib::tools::ToolsCapability,
            &harn_hostlib::fs::FsCapability,
        ]);
        let mut errors = Vec::new();
        for name in ["hostlib_tools_read_file", "hostlib_fs_read_text"] {
            let read = registry.find(name).unwrap();
            let result = (read.handler)(&dict_arg(&[
                ("path", vm_string(&path_str(&fifo))),
                ("session_id", vm_string(&session)),
            ]));
            errors.push(result.err().map(|error| error.to_string()));
        }
        sender.send(errors).unwrap();
    });
    let result = std::panic::catch_unwind(|| {
        harn_clock::test_support::recv_within("FIFO reader's regular-file refusal", &receiver)
    });
    drop(keeper);
    worker.join().unwrap();
    let errors = result.unwrap();
    assert_eq!(errors.len(), 2);
    for error in errors {
        let error = error.expect("FIFO is not a regular file");
        assert!(error.contains("not a regular file"), "{error}");
    }
    let path = dir.path().join("other-session");
    fs::write(&path, b"other session remains usable").unwrap();
    let registry = tools_registry();
    let read = registry.find("hostlib_tools_read_file").unwrap();
    let result = (read.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&path))),
        ("session_id", vm_string(&uuid::Uuid::new_v4().to_string())),
    ]))
    .unwrap();
    assert!(
        matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_str() == "other session remains usable")
    );
}

#[test]
fn read_file_binary_returns_base64() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("bin.dat");
    fs::write(&file, [0u8, 1, 2, 3, 0xff]).unwrap();

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_read_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("encoding", vm_string("binary")),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&result, "encoding"), VmValue::String(s) if s.as_str() == "base64"));
}

#[test]
fn read_file_invalid_utf8_falls_back_to_base64() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("notutf8.bin");
    fs::write(&file, [0xff, 0xfe, 0xfd]).unwrap();

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_read_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[("path", vm_string(&path_str(&file)))])).unwrap();
    assert!(matches!(dict_get(&result, "encoding"), VmValue::String(s) if s.as_str() == "base64"));
}

#[test]
fn read_file_errors_when_missing() {
    let dir = TempDir::new().unwrap();
    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_read_file").unwrap();
    let err = (entry.handler)(&dict_arg(&[(
        "path",
        vm_string(&path_str(&dir.path().join("nope"))),
    )]))
    .unwrap_err();
    assert!(matches!(err, HostlibError::Backend { .. }));
}

#[test]
fn write_file_creates_file_and_parent_directories() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("nested/deep/note.txt");

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_write_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&nested))),
        ("content", vm_string("welcome")),
    ]))
    .unwrap();

    assert!(matches!(dict_get(&result, "created"), VmValue::Bool(true)));
    assert!(matches!(
        dict_get(&result, "bytes_written"),
        VmValue::Int(7)
    ));
    assert_eq!(fs::read_to_string(&nested).unwrap(), "welcome");
}

#[test]
fn write_file_respects_overwrite_false() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("locked.txt");
    fs::write(&file, "existing").unwrap();

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_write_file").unwrap();
    let err = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("new")),
        ("overwrite", VmValue::Bool(false)),
    ]))
    .unwrap_err();
    assert!(matches!(err, HostlibError::Backend { .. }));
    assert_eq!(fs::read_to_string(&file).unwrap(), "existing");
}

#[test]
fn write_file_base64_decodes_payload() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("binary.bin");

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_write_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("aGVsbG8=")), // "hello"
        ("encoding", vm_string("base64")),
    ]))
    .unwrap();
    assert!(matches!(
        dict_get(&result, "bytes_written"),
        VmValue::Int(5)
    ));
    assert_eq!(fs::read(&file).unwrap(), b"hello");
}

#[test]
fn delete_file_missing_returns_removed_false() {
    let dir = TempDir::new().unwrap();
    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_delete_file").unwrap();
    let result = (entry.handler)(&dict_arg(&[(
        "path",
        vm_string(&path_str(&dir.path().join("absent"))),
    )]))
    .unwrap();
    assert!(matches!(dict_get(&result, "removed"), VmValue::Bool(false)));
}

#[test]
fn delete_file_directory_requires_recursive() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("a/b");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("inner.txt"), "x").unwrap();

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_delete_file").unwrap();
    let err = (entry.handler)(&dict_arg(&[(
        "path",
        vm_string(&path_str(&dir.path().join("a"))),
    )]))
    .unwrap_err();
    assert!(matches!(err, HostlibError::Backend { .. }));

    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&dir.path().join("a")))),
        ("recursive", VmValue::Bool(true)),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&result, "removed"), VmValue::Bool(true)));
}

#[test]
fn list_directory_returns_entries_sorted() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    fs::write(dir.path().join("b.txt"), "b").unwrap();
    fs::create_dir(dir.path().join("c_dir")).unwrap();
    fs::write(dir.path().join(".hidden"), "h").unwrap();

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_list_directory").unwrap();
    let result = (entry.handler)(&dict_arg(&[("path", vm_string(&path_str(dir.path())))])).unwrap();
    let entries = match dict_get(&result, "entries") {
        VmValue::List(l) => l.clone(),
        other => panic!("expected list, got {other:?}"),
    };
    let names: Vec<String> = entries
        .iter()
        .map(|v| match dict_get(v, "name") {
            VmValue::String(s) => s.to_string(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(names, vec!["a.txt", "b.txt", "c_dir"]);
}

#[test]
fn list_directory_include_hidden_works() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("visible"), "v").unwrap();
    fs::write(dir.path().join(".hidden"), "h").unwrap();

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_list_directory").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(dir.path()))),
        ("include_hidden", VmValue::Bool(true)),
    ]))
    .unwrap();
    if let VmValue::List(rows) = dict_get(&result, "entries") {
        assert_eq!(rows.len(), 2);
    } else {
        panic!("expected entries list");
    }
}

#[cfg(unix)]
#[test]
fn list_directory_reports_symlink_itself_without_following_target() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let target = outside.path().join("secret.txt");
    fs::write(&target, "secret payload").unwrap();
    let target_size = fs::metadata(&target).unwrap().len() as i64;
    let link = dir.path().join("link.txt");
    symlink(&target, &link).unwrap();

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_list_directory").unwrap();
    let result = (entry.handler)(&dict_arg(&[("path", vm_string(&path_str(dir.path())))])).unwrap();
    let entries = match dict_get(&result, "entries") {
        VmValue::List(rows) => rows,
        other => panic!("expected list, got {other:?}"),
    };
    assert_eq!(entries.len(), 1);
    assert!(matches!(
        dict_get(&entries[0], "is_symlink"),
        VmValue::Bool(true)
    ));
    assert!(matches!(dict_get(&entries[0], "size"), VmValue::Int(size) if *size != target_size));
}

#[test]
fn list_directory_respects_max_entries_and_marks_truncated() {
    let dir = TempDir::new().unwrap();
    for i in 0..5 {
        fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
    }

    let reg = tools_registry();
    let entry = reg.find("hostlib_tools_list_directory").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("path", vm_string(&path_str(dir.path()))),
        ("max_entries", VmValue::Int(2)),
    ]))
    .unwrap();
    if let VmValue::List(rows) = dict_get(&result, "entries") {
        assert_eq!(rows.len(), 2);
    }
    assert!(matches!(
        dict_get(&result, "truncated"),
        VmValue::Bool(true)
    ));
}
