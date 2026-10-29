use super::*;

#[test]
fn go_package_rename_refuses_unproven_bindings_without_writes() {
    let cases: &[(&str, &[(&str, &str)], &str, &str, &str)] = &[
        (
            "sibling dot import",
            &[
                ("store.go", "package lab\nfunc Setup() int { return 1 }\n"),
                ("use.go", "package lab\nvar _ = Setup()\n"),
                ("dot.go", "package lab\nimport . \"strings\"\nvar _ = ToUpper(\"x\")\n"),
            ],
            "ToLower",
            "workspace",
            "refused",
        ),
        (
            "dot-importing consumer",
            &[
                ("store.go", "package lab\nfunc Setup() int { return 1 }\nfunc Other() int { return 2 }\n"),
                ("cmd/main.go", "package main\nimport . \"example.com/lab\"\nfunc Prepare() {}\nfunc main() { _ = Other() }\n"),
            ],
            "Prepare",
            "workspace",
            "ambiguous_symbol",
        ),
        (
            "file scope",
            &[("store.go", "package lab\nfunc Setup() int { return 1 }\n"), ("use.go", "package lab\nvar _ = Setup()\n")],
            "Prepare",
            "file",
            "ambiguous_symbol",
        ),
        (
            "module scope",
            &[("store.go", "package lab\nfunc Setup() int { return 1 }\n"), ("use.go", "package lab\nvar _ = Setup()\n")],
            "Prepare",
            "module",
            "ambiguous_symbol",
        ),
        ("init", &[("store.go", "package lab\nfunc Setup() int { return 1 }\n")], "init", "workspace", "invalid_identifier"),
        ("blank", &[("store.go", "package lab\nfunc Setup() int { return 1 }\n")], "_", "workspace", "invalid_identifier"),
    ];
    let mut failures = Vec::new();
    for (label, files, new_name, scope, expected) in cases {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("go.mod"), "module example.com/lab\n\ngo 1.23\n").unwrap();
        for (path, source) in *files {
            let absolute = root.join(path);
            fs::create_dir_all(absolute.parent().unwrap()).unwrap();
            fs::write(absolute, source).unwrap();
        }
        let capability = build_index(root);
        let symbol = dict(&[
            ("name", vm_string("Setup")),
            ("path", vm_string("store.go")),
            ("kind", vm_string("Function")),
        ]);
        let result = rename(&capability, symbol, new_name, scope);
        if s(field(&result, "result")) != *expected
            || files
                .iter()
                .any(|(path, source)| fs::read_to_string(root.join(path)).unwrap() != *source)
        {
            failures.push(format!("{label}: {result:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "the expected refusal or unchanged sources were missing: {failures:#?}"
    );
}

#[test]
fn go_narrow_rename_applies_when_no_binding_is_left_outside_scope() {
    for scope in ["file", "module"] {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let source = "package lab\nfunc Setup() int { return 1 }\n";
        fs::write(root.join("store.go"), source).unwrap();
        fs::write(root.join("other.go"), "package lab\nvar unrelated = 2\n").unwrap();
        let capability = build_index(root);
        let symbol = dict(&[
            ("name", vm_string("Setup")),
            ("path", vm_string("store.go")),
            ("kind", vm_string("Function")),
        ]);
        let result = rename(&capability, symbol, "Prepare", scope);
        assert_eq!(
            s(field(&result, "result")),
            "applied",
            "{scope}: {result:?}"
        );
        assert_eq!(
            fs::read_to_string(root.join("store.go")).unwrap(),
            source.replace("Setup", "Prepare")
        );
    }
}

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
