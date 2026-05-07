use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HardwareSnapshot {
    pub ram: RamSnapshot,
    pub gpu: GpuSnapshot,
    pub disk: DiskSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RamSnapshot {
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GpuSnapshot {
    pub kind: GpuKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GpuKind {
    None,
    #[allow(dead_code)]
    Mps,
    Cuda,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DiskSnapshot {
    pub path: PathBuf,
    pub free_bytes: Option<u64>,
}

pub(crate) fn collect_hardware_snapshot() -> HardwareSnapshot {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    HardwareSnapshot {
        ram: detect_ram(),
        gpu: detect_gpu(),
        disk: detect_disk(&cwd),
    }
}

pub(crate) fn bytes_to_gib_rounded(bytes: u64) -> u64 {
    (bytes + (GIB / 2)) / GIB
}

pub(crate) fn bytes_to_gib_floor(bytes: u64) -> u64 {
    bytes / GIB
}

fn detect_ram() -> RamSnapshot {
    detect_ram_platform().unwrap_or(RamSnapshot {
        total_bytes: None,
        available_bytes: None,
    })
}

#[cfg(target_os = "linux")]
fn detect_ram_platform() -> Option<RamSnapshot> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_linux_meminfo(&text)
}

#[cfg(target_os = "macos")]
fn detect_ram_platform() -> Option<RamSnapshot> {
    let total = command_stdout("sysctl", &["-n", "hw.memsize"])
        .and_then(|text| text.trim().parse::<u64>().ok());
    let available = command_stdout("vm_stat", &[]).and_then(|text| parse_macos_vm_stat(&text));
    Some(RamSnapshot {
        total_bytes: total,
        available_bytes: available,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn detect_ram_platform() -> Option<RamSnapshot> {
    None
}

fn detect_gpu() -> GpuSnapshot {
    #[cfg(target_os = "macos")]
    {
        if Path::new("/System/Library/Frameworks/MetalPerformanceShaders.framework").exists() {
            return GpuSnapshot { kind: GpuKind::Mps };
        }
    }

    if Command::new("nvidia-smi")
        .arg("-L")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return GpuSnapshot {
            kind: GpuKind::Cuda,
        };
    }

    GpuSnapshot {
        kind: GpuKind::None,
    }
}

fn detect_disk(path: &Path) -> DiskSnapshot {
    DiskSnapshot {
        path: path.to_path_buf(),
        free_bytes: fs2::free_space(path).ok(),
    }
}

#[cfg(target_os = "macos")]
fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_meminfo(text: &str) -> Option<RamSnapshot> {
    let mut total_kib = None;
    let mut available_kib = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("MemTotal:") => total_kib = parts.next().and_then(|value| value.parse().ok()),
            Some("MemAvailable:") => {
                available_kib = parts.next().and_then(|value| value.parse().ok())
            }
            _ => {}
        }
    }
    Some(RamSnapshot {
        total_bytes: total_kib.map(|kib: u64| kib * 1024),
        available_bytes: available_kib.map(|kib: u64| kib * 1024),
    })
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos_vm_stat(text: &str) -> Option<u64> {
    let page_size = parse_page_size(text)?;
    let mut pages = 0u64;
    for line in text.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if matches!(
            name.trim(),
            "Pages free" | "Pages inactive" | "Pages speculative"
        ) {
            pages = pages.saturating_add(parse_page_count(value)?);
        }
    }
    Some(pages.saturating_mul(page_size))
}

#[cfg(any(target_os = "macos", test))]
fn parse_page_size(text: &str) -> Option<u64> {
    let first = text.lines().next()?;
    let start = first.find("page size of ")? + "page size of ".len();
    let tail = &first[start..];
    let end = tail.find(" bytes")?;
    tail[..end].trim().parse().ok()
}

#[cfg(any(target_os = "macos", test))]
fn parse_page_count(value: &str) -> Option<u64> {
    value.trim().trim_end_matches('.').parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{bytes_to_gib_rounded, parse_linux_meminfo, parse_macos_vm_stat, RamSnapshot, GIB};

    #[test]
    fn linux_meminfo_reports_total_and_available_bytes() {
        let snapshot = parse_linux_meminfo(
            "MemTotal:       16384000 kB\nMemFree:         1000000 kB\nMemAvailable:    8192000 kB\n",
        )
        .expect("meminfo parses");
        assert_eq!(
            snapshot,
            RamSnapshot {
                total_bytes: Some(16_384_000 * 1024),
                available_bytes: Some(8_192_000 * 1024),
            }
        );
    }

    #[test]
    fn macos_vm_stat_counts_reclaimable_pages() {
        let available = parse_macos_vm_stat(
            "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
             Pages free:                               10.\n\
             Pages active:                             20.\n\
             Pages inactive:                           30.\n\
             Pages speculative:                        40.\n",
        )
        .expect("vm_stat parses");
        assert_eq!(available, 80 * 16_384);
    }

    #[test]
    fn gib_formatting_rounds_to_nearest_gib() {
        assert_eq!(bytes_to_gib_rounded(8 * GIB), 8);
        assert_eq!(bytes_to_gib_rounded(8 * GIB + GIB / 2), 9);
    }
}
