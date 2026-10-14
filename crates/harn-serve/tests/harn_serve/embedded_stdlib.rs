#[test]
fn server_assembly_installs_stdlib_in_a_fresh_host() {
    const CHILD: &str = "EMBEDDED_STDLIB_ASSEMBLY_PROBE";
    if std::env::var_os(CHILD).is_some() {
        assert!(
            harn_stdlib_catalog::installed().is_err(),
            "the child must start without another host's installation"
        );
        let _server = harn_serve::AcpServer::new(harn_serve::AcpServerConfig::new(None));
        let catalog = harn_stdlib_catalog::installed()
            .expect("the real server constructor installs the catalog");
        assert!(catalog
            .source("agent/loop")
            .is_some_and(|source| !source.is_empty()));
        let _second = harn_serve::AcpServer::new(harn_serve::AcpServerConfig::new(None));
        assert!(
            std::ptr::eq(catalog, harn_stdlib_catalog::installed().unwrap()),
            "multiple server instances reuse one catalog"
        );
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "embedded_stdlib::server_assembly_installs_stdlib_in_a_fresh_host",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .status()
        .expect("start the isolated host probe");
    assert!(
        status.success(),
        "the host assembly probe must execute and succeed"
    );
}
