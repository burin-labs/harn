use std::env;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

fn write_stdout(value: impl AsRef<[u8]>) {
    io::stdout()
        .write_all(value.as_ref())
        .expect("write test helper output");
}

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("--cwd") => {
            let cwd = env::current_dir()
                .expect("read test helper cwd")
                .canonicalize()
                .expect("canonicalize test helper cwd");
            write_stdout(cwd.to_string_lossy().as_bytes());
        }
        Some("--env") => {
            let values = args
                .map(|name| env::var(name).unwrap_or_default())
                .collect::<Vec<_>>();
            write_stdout(values.join("|"));
        }
        Some("--sleep-ms") => {
            let millis = args
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default();
            thread::sleep(Duration::from_millis(millis));
        }
        #[cfg(target_os = "linux")]
        Some("--proc-self-maps") => {
            let maps =
                std::fs::read_to_string("/proc/self/maps").expect("read this process's memory map");
            assert!(
                !maps.is_empty(),
                "this process's memory map must not be empty"
            );
            assert!(
                std::fs::read("/proc/self/environ").is_err(),
                "the narrow maps grant must not expose this process's environment"
            );
            write_stdout("maps-readable|environ-denied");
        }
        Some(name) => {
            write_stdout(env::var(name).unwrap_or_default());
        }
        None => {}
    }
}
