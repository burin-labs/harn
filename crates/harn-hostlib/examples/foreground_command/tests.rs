use std::process::Command;

pub(super) fn run() {
    #[cfg(unix)]
    {
        let exe = std::env::current_exe().unwrap();
        let positive = Command::new(&exe)
            .args(["--client", "/bin/echo", "foreground-embedding-ok"])
            .output()
            .unwrap();
        assert!(positive.status.success(), "{positive:?}");
        assert_eq!(positive.stdout, b"foreground-embedding-ok\n");
        assert!(positive.stderr.is_empty(), "{positive:?}");

        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("payload-reached");
        let negative = Command::new(&exe)
            .env("HOSTLIB_TEST_WITHOUT_GUARDIAN", "1")
            .args(["--client", "/usr/bin/touch"])
            .arg(&marker)
            .output()
            .unwrap();
        assert!(!negative.status.success(), "{negative:?}");
        assert!(negative.stdout.is_empty(), "{negative:?}");
        let stderr = String::from_utf8(negative.stderr).unwrap();
        assert!(
            stderr.contains("run_if_requested at the start of main"),
            "{stderr}"
        );
        assert!(!marker.exists(), "uninitialized client reached the payload");

        let control = Command::new(&exe)
            .args(["--client", "/usr/bin/touch"])
            .arg(&marker)
            .output()
            .unwrap();
        assert!(control.status.success(), "{control:?}");
        assert!(control.stdout.is_empty(), "{control:?}");
        assert!(control.stderr.is_empty(), "{control:?}");
        assert!(
            marker.is_file(),
            "initialized control did not reach the payload"
        );
        println!("foreground_embedding observed=3 passed=3 failed=0 pending=0");
    }
}
