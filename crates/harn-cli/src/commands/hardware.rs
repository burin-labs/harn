//! Single owner for host hardware facts.
//!
//! Every command that reports RAM, GPU, or free disk reads them from here
//! (`harn doctor`, `harn quickstart`, `harn local *`, `harn models recommend`)
//! so the numbers they print cannot drift apart. RAM totals and disk space come
//! from `sysinfo`; GPU still shells out, because `sysinfo` does not model
//! accelerators.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use sysinfo::{Disks, MemoryRefreshKind, RefreshKind, System};

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
    pub total_memory_bytes: Option<u64>,
    pub free_memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GpuKind {
    None,
    Mps,
    Cuda,
}

impl GpuKind {
    /// Human-readable label for report surfaces such as `harn doctor`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            GpuKind::None => "CPU-only",
            GpuKind::Mps => "Apple Silicon (MPS available)",
            GpuKind::Cuda => "NVIDIA GPU detected",
        }
    }
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

pub(crate) fn bytes_to_gib_f64(bytes: u64) -> f64 {
    bytes as f64 / GIB as f64
}

pub(crate) fn detect_ram() -> RamSnapshot {
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    RamSnapshot {
        total_bytes: nonzero(system.total_memory()),
        available_bytes: available_ram_bytes(&system),
    }
}

/// Everywhere but macOS, `sysinfo` reports the platform's own "memory you
/// could still allocate" figure — `MemAvailable` on Linux — which is exactly
/// what callers want, so defer to it.
#[cfg(not(target_os = "macos"))]
fn available_ram_bytes(system: &System) -> Option<u64> {
    nonzero(system.available_memory())
}

/// macOS is the deliberate exception: do NOT route this through
/// `sysinfo::available_memory()`, and do not "finish the migration" by doing
/// so later. That figure is `active + inactive + free` (see sysinfo
/// `src/unix/apple/system.rs`), i.e. pages processes are *currently resident
/// in* — a different quantity from "pages we could reclaim under memory
/// pressure". Measured on a 48 GiB machine under load it read 32.5 GiB against
/// the 16.5 GiB this reclaimable estimate reports: roughly 2x.
///
/// Two call sites treat this number as a headroom budget, and doubling it
/// silently disables both:
///   - `commands/local/launch.rs` refuses to launch a model when
///     `estimate + safety_margin > available` — an inflated `available` turns
///     that refusal into a green light on the exact machines the guard exists
///     to protect.
///   - `commands/models/recommend.rs` (`ram_bucket_from_available_bytes`)
///     buckets on it, so an inflated value recommends a larger local model
///     than actually fits.
///
/// So keep the conservative `vm_stat` reclaimable-pages estimate on macOS.
#[cfg(target_os = "macos")]
fn available_ram_bytes(_system: &System) -> Option<u64> {
    command_stdout("vm_stat", &[]).and_then(|text| parse_macos_vm_stat(&text))
}

pub(crate) fn detect_gpu() -> GpuSnapshot {
    if apple_silicon_detected() {
        return GpuSnapshot {
            kind: GpuKind::Mps,
            total_memory_bytes: None,
            free_memory_bytes: None,
        };
    }

    if let Some((total, free)) = detect_nvidia_memory_bytes() {
        return GpuSnapshot {
            kind: GpuKind::Cuda,
            total_memory_bytes: Some(total),
            free_memory_bytes: Some(free),
        };
    }

    // A CUDA host without `nvidia-smi` on PATH still exposes the device node;
    // we learn the kind but not the memory.
    if Path::new("/dev/nvidia0").exists() {
        return GpuSnapshot {
            kind: GpuKind::Cuda,
            total_memory_bytes: None,
            free_memory_bytes: None,
        };
    }

    GpuSnapshot {
        kind: GpuKind::None,
        total_memory_bytes: None,
        free_memory_bytes: None,
    }
}

/// Free space on the volume backing `path`, as `sysinfo` reports it. The exact
/// figure is the platform's "space you could realistically write" number, which
/// differs from the raw free-block count the old `df`/`fs2` probes returned:
/// on Linux/BSD it is `f_bavail` (excludes root-reserved blocks); on macOS it is
/// `AvailableCapacityForImportantUsage`, which additionally counts purgeable
/// space (caches, local snapshots) the OS reclaims under pressure — so on macOS
/// this reads *higher* than `df`, matching Finder's "Available".
pub(crate) fn detect_disk(path: &Path) -> DiskSnapshot {
    DiskSnapshot {
        path: path.to_path_buf(),
        free_bytes: available_space_for(path),
    }
}

fn available_space_for(path: &Path) -> Option<u64> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    // On Windows `canonicalize` returns an extended-length `\\?\` verbatim path,
    // whose prefix component never `starts_with`-matches the plain `C:\` mount
    // points sysinfo reports, so no disk would match and free space would read
    // as `None` on every Windows machine. De-verbatim (via the shared owner in
    // `harn_vm::windows_path`) before comparing.
    let probe = PathBuf::from(
        harn_vm::windows_path::strip_windows_verbatim_prefix(&canonical.to_string_lossy())
            .into_owned(),
    );
    Disks::new_with_refreshed_list()
        .iter()
        // Nested mounts all prefix-match `probe`; the deepest one owns it.
        .filter(|disk| probe.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space())
}

#[cfg(target_os = "macos")]
fn apple_silicon_detected() -> bool {
    command_stdout("sysctl", &["-n", "hw.optional.arm64"]).is_some_and(|value| value.trim() == "1")
}

#[cfg(not(target_os = "macos"))]
fn apple_silicon_detected() -> bool {
    false
}

fn detect_nvidia_memory_bytes() -> Option<(u64, u64)> {
    let text = command_stdout(
        "nvidia-smi",
        &[
            "--query-gpu=memory.total,memory.free",
            "--format=csv,noheader,nounits",
        ],
    )?;
    parse_nvidia_memory_csv(&text)
}

fn parse_nvidia_memory_csv(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|line| !line.trim().is_empty())?;
    let mut parts = line.split(',').map(str::trim);
    let total_mib = parts.next()?.parse::<u64>().ok()?;
    let free_mib = parts.next()?.parse::<u64>().ok()?;
    Some((total_mib * 1024 * 1024, free_mib * 1024 * 1024))
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
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

fn nonzero(value: u64) -> Option<u64> {
    (value > 0).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::{
        bytes_to_gib_rounded, detect_disk, detect_ram, parse_macos_vm_stat,
        parse_nvidia_memory_csv, GpuKind, GIB,
    };

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
        // Deliberately excludes the 20 active pages: this is a headroom budget,
        // not a "how much RAM exists" figure.
        assert_eq!(available, 80 * 16_384);
    }

    #[test]
    fn gib_formatting_rounds_to_nearest_gib() {
        assert_eq!(bytes_to_gib_rounded(8 * GIB), 8);
        assert_eq!(bytes_to_gib_rounded(8 * GIB + GIB / 2), 9);
    }

    #[test]
    fn nvidia_memory_csv_reports_first_gpu_bytes() {
        assert_eq!(
            parse_nvidia_memory_csv("32607, 20480\n24576, 12000\n"),
            Some((32607 * 1024 * 1024, 20480 * 1024 * 1024))
        );
    }

    #[test]
    fn gpu_kind_labels_match_the_documented_doctor_strings() {
        assert_eq!(GpuKind::None.label(), "CPU-only");
        assert_eq!(GpuKind::Mps.label(), "Apple Silicon (MPS available)");
        assert_eq!(GpuKind::Cuda.label(), "NVIDIA GPU detected");
    }

    #[test]
    fn ram_detection_reports_a_plausible_total() {
        let ram = detect_ram();
        let total = ram.total_bytes.expect("host reports total RAM");
        assert!(total >= GIB, "total RAM {total} bytes looks too small");
        if let Some(available) = ram.available_bytes {
            assert!(
                available <= total,
                "available RAM {available} exceeds total {total}"
            );
        }
    }

    #[test]
    fn disk_detection_resolves_the_volume_backing_a_path() {
        let cwd = std::env::current_dir().expect("cwd readable");
        let disk = detect_disk(&cwd);
        assert_eq!(disk.path, cwd);
        assert!(
            disk.free_bytes.is_some(),
            "cwd should resolve to a mounted volume"
        );
    }
}
