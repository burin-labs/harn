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
            let protected_pid = args.next().expect("protected parent pid");
            let maps =
                std::fs::read_to_string("/proc/self/maps").expect("read this process's memory map");
            assert!(
                !maps.is_empty(),
                "this process's memory map must not be empty"
            );
            let stack_probe = 0_u8;
            let stack_address = (&raw const stack_probe) as usize;
            assert!(
                maps.lines().any(|line| {
                    let Some((range, _)) = line.split_once(' ') else {
                        return false;
                    };
                    let Some((start, end)) = range.split_once('-') else {
                        return false;
                    };
                    let Ok(start) = usize::from_str_radix(start, 16) else {
                        return false;
                    };
                    let Ok(end) = usize::from_str_radix(end, 16) else {
                        return false;
                    };
                    start <= stack_address && stack_address < end
                }),
                "the memory map must describe this process's current stack"
            );
            assert!(
                std::fs::read(format!("/proc/{protected_pid}/environ")).is_err(),
                "the runtime proc grant must not expose the host parent's environment"
            );
            write_stdout("maps-readable|parent-environ-denied");
        }
        #[cfg(target_os = "linux")]
        Some("--proc-self-maps-child") => {
            let protected_pid = args.next().expect("protected parent pid");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--proc-self-maps")
                .arg(protected_pid)
                .output()
                .expect("spawn maps probe child");
            assert!(
                output.status.success(),
                "maps probe child failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            write_stdout("child-maps-readable|parent-environ-denied");
        }
        Some(name) => {
            write_stdout(env::var(name).unwrap_or_default());
        }
        None => {}
    }
}
