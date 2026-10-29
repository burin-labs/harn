use super::*;

#[test]
fn go_rename_refuses_import_alias_in_seed_or_sibling() {
    for (alias_in_seed, import) in [
        (false, "import url \"net/url\"\nvar _ = url.URL{}\n"),
        (true, "import url \"net/url\"\nvar _ = url.URL{}\n"),
        (false, "import \"net/url\"\nvar _ = url.URL{}\n"),
    ] {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let declaration = format!(
            "package lab\n{}\ntype DocumentStore struct{{}}\n",
            if alias_in_seed { import } else { "" }
        );
        let sibling = format!("package lab\n{}", if alias_in_seed { "" } else { import });
        fs::write(root.join("store.go"), &declaration).unwrap();
        fs::write(root.join("other.go"), &sibling).unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("DocumentStore")),
            ("path", vm_string("store.go")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol_ref, "url", "workspace");
        assert_eq!(s(field(&result, "result")), "conflict", "{result:?}");
        assert!(list_len(field(&result, "conflicts")) >= 1);
        assert_eq!(
            fs::read_to_string(root.join("store.go")).unwrap(),
            declaration
        );
        assert_eq!(fs::read_to_string(root.join("other.go")).unwrap(), sibling);
    }
}

#[test]
fn go_rename_refuses_uninspectable_sibling_without_writing() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let declaration = "package lab\n\ntype DocumentStore struct{}\n";
    let sibling = "package lab\nfunc broken(\n";
    fs::write(root.join("store.go"), declaration).unwrap();
    fs::write(root.join("other.go"), sibling).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("DocumentStore")),
        ("path", vm_string("store.go")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "DocumentLibrary", "workspace");
    assert_eq!(s(field(&result, "result")), "refused", "{result:?}");
    assert!(s(field(&result, "details")).contains("does not parse"));
    assert_eq!(
        fs::read_to_string(root.join("store.go")).unwrap(),
        declaration
    );
    assert_eq!(fs::read_to_string(root.join("other.go")).unwrap(), sibling);
}

#[test]
fn go_rename_allows_same_name_in_external_test_package() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("lab")).unwrap();
    let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
    let external = "package lab_test\n\ntype DocumentLibrary struct { Name string }\n";
    fs::write(root.join("lab/store.go"), declaration).unwrap();
    fs::write(root.join("lab/other_test.go"), external).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("DocumentStore")),
        ("path", vm_string("lab/store.go")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "DocumentLibrary", "workspace");
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(
        fs::read_to_string(root.join("lab/store.go")).unwrap(),
        declaration.replace("DocumentStore", "DocumentLibrary")
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/other_test.go")).unwrap(),
        external
    );
}

#[test]
fn go_rename_allows_same_spelling_in_a_sibling_field() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("lab")).unwrap();
    let declaration = "package lab\n\ntype DocumentStore struct { Count int }\n";
    let sibling = "package lab\n\ntype Settings struct { DocumentLibrary int }\n";
    fs::write(root.join("lab/store.go"), declaration).unwrap();
    fs::write(root.join("lab/other.go"), sibling).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("DocumentStore")),
        ("path", vm_string("lab/store.go")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "DocumentLibrary", "workspace");
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(
        fs::read_to_string(root.join("lab/store.go")).unwrap(),
        declaration.replace("DocumentStore", "DocumentLibrary")
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/other.go")).unwrap(),
        sibling
    );
}

#[test]
fn go_workspace_rename_refuses_same_name_in_filename_build_variants() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("lab")).unwrap();
    let linux = "package lab\n\ntype DocumentStore struct { Linux int }\n";
    let windows = "package lab\n\ntype DocumentStore struct { Windows int }\n";
    let usage = "package lab\n\nfunc Read(s *DocumentStore) *DocumentStore { return s }\n";
    fs::write(root.join("lab/store_linux.go"), linux).unwrap();
    fs::write(root.join("lab/store_windows.go"), windows).unwrap();
    fs::write(root.join("lab/use.go"), usage).unwrap();
    let capability = build_index(root);
    let symbol_ref = dict(&[
        ("name", vm_string("DocumentStore")),
        ("path", vm_string("lab/store_linux.go")),
        ("kind", vm_string("Type")),
    ]);
    let result = rename(&capability, symbol_ref, "DocumentLibrary", "workspace");
    assert_eq!(
        s(field(&result, "result")),
        "ambiguous_symbol",
        "{result:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/store_linux.go")).unwrap(),
        linux
    );
    assert_eq!(
        fs::read_to_string(root.join("lab/store_windows.go")).unwrap(),
        windows
    );
    assert_eq!(fs::read_to_string(root.join("lab/use.go")).unwrap(), usage);
}
