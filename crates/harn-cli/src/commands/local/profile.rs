//! Machine profiles for `harn local switch`.
//!
//! A 48 GB Apple Silicon laptop should default to a bigger context window
//! and a more aggressive keep-alive than a low-RAM Linux box without an
//! accelerator. We derive both from the existing [`HardwareSnapshot`] so
//! `harn local switch` produces sensible defaults without per-machine
//! configuration.

use crate::cli::LocalProfileArgs;
use crate::commands::hardware::{
    bytes_to_gib_f64, bytes_to_gib_floor, collect_hardware_snapshot, GpuKind, HardwareSnapshot,
};
use harn_vm::llm::local_profiles::RuntimeProfileHost;

/// Recommended defaults the user can opt out of with `--ctx` / `--keep-alive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalDefaults {
    pub ctx: u64,
    pub keep_alive: &'static str,
    pub bucket: ProfileBucket,
}

/// Coarse machine tier. Wider buckets keep the table small enough to stay
/// inspectable without a full HW database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileBucket {
    /// <=16 GB total RAM. Treat the laptop as memory-constrained.
    LowRam,
    /// 17-32 GB total RAM, no accelerator.
    MidRamNoGpu,
    /// 17-32 GB total RAM with an MPS or CUDA accelerator.
    MidRamAccel,
    /// >=33 GB total RAM, no accelerator. Most desktop Linux boxes.
    HighRamNoGpu,
    /// >=33 GB total RAM with an MPS or CUDA accelerator (e.g. 48 GB MacBook).
    HighRamAccel,
}

impl ProfileBucket {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::LowRam => "low_ram",
            Self::MidRamNoGpu => "mid_ram_no_gpu",
            Self::MidRamAccel => "mid_ram_accel",
            Self::HighRamNoGpu => "high_ram_no_gpu",
            Self::HighRamAccel => "high_ram_accel",
        }
    }
}

pub(crate) fn defaults_for(hardware: &HardwareSnapshot) -> LocalDefaults {
    let bucket = bucket_for(hardware);
    match bucket {
        ProfileBucket::LowRam => LocalDefaults {
            ctx: 8_192,
            keep_alive: "5m",
            bucket,
        },
        ProfileBucket::MidRamNoGpu => LocalDefaults {
            ctx: 16_384,
            keep_alive: "10m",
            bucket,
        },
        ProfileBucket::MidRamAccel => LocalDefaults {
            ctx: 32_768,
            keep_alive: "30m",
            bucket,
        },
        ProfileBucket::HighRamNoGpu => LocalDefaults {
            ctx: 32_768,
            keep_alive: "30m",
            bucket,
        },
        ProfileBucket::HighRamAccel => LocalDefaults {
            ctx: 65_536,
            keep_alive: "1h",
            bucket,
        },
    }
}

pub(crate) fn run(args: LocalProfileArgs) -> Result<(), String> {
    let hardware = collect_hardware_snapshot();
    let resolved = harn_vm::llm_config::resolve_model_info(&args.model);
    let provider = args
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .unwrap_or(&resolved.provider);
    let report = harn_vm::llm::local_profiles::local_runtime_profile_report_for_host(
        resolved.alias.as_deref(),
        &resolved.id,
        provider,
        Some(runtime_profile_host(&hardware)),
    );
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("failed to render local profile JSON: {error}"))?
        );
    } else {
        println!(
            "{} via {}: {}",
            report.model_id,
            report.provider,
            report.selected_status.as_str()
        );
        if !report.selected.requires.is_empty() {
            println!("  required probes: {}", report.selected.requires.join(", "));
        }
        if let Some(ctx) = report.selected.recommended_num_ctx {
            println!("  recommended ctx: {ctx}");
        }
        for risk in &report.selected.known_risks {
            println!("  risk: {risk}");
        }
        for workaround in &report.selected.workarounds {
            println!("  workaround: {workaround}");
        }
    }
    Ok(())
}

pub(crate) fn runtime_profile_host(hardware: &HardwareSnapshot) -> RuntimeProfileHost {
    RuntimeProfileHost {
        system_available_gib: hardware.ram.available_bytes.map(bytes_to_gib_f64),
        accelerator_free_gib: hardware.gpu.free_memory_bytes.map(bytes_to_gib_f64),
    }
}

fn bucket_for(hardware: &HardwareSnapshot) -> ProfileBucket {
    let total_gib = hardware
        .ram
        .total_bytes
        .map(bytes_to_gib_floor)
        .unwrap_or(0);
    let has_accel = !matches!(hardware.gpu.kind, GpuKind::None);
    match (total_gib, has_accel) {
        (0..=16, _) => ProfileBucket::LowRam,
        (17..=32, false) => ProfileBucket::MidRamNoGpu,
        (17..=32, true) => ProfileBucket::MidRamAccel,
        (_, false) => ProfileBucket::HighRamNoGpu,
        (_, true) => ProfileBucket::HighRamAccel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::hardware::{DiskSnapshot, GpuSnapshot, RamSnapshot};
    use std::path::PathBuf;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn snapshot(total_gib: u64, gpu: GpuKind) -> HardwareSnapshot {
        HardwareSnapshot {
            ram: RamSnapshot {
                total_bytes: Some(total_gib * GIB),
                available_bytes: Some(total_gib * GIB / 2),
            },
            gpu: GpuSnapshot {
                kind: gpu,
                total_memory_bytes: None,
                free_memory_bytes: None,
            },
            disk: DiskSnapshot {
                path: PathBuf::from("/tmp"),
                free_bytes: Some(128 * GIB),
            },
        }
    }

    #[test]
    fn low_ram_box_uses_small_ctx_and_short_keep_alive() {
        let defaults = defaults_for(&snapshot(8, GpuKind::None));
        assert_eq!(defaults.bucket, ProfileBucket::LowRam);
        assert_eq!(defaults.ctx, 8_192);
        assert_eq!(defaults.keep_alive, "5m");
    }

    #[test]
    fn apple_silicon_48gb_uses_wide_ctx_and_one_hour_keep_alive() {
        let defaults = defaults_for(&snapshot(48, GpuKind::Mps));
        assert_eq!(defaults.bucket, ProfileBucket::HighRamAccel);
        assert_eq!(defaults.ctx, 65_536);
        assert_eq!(defaults.keep_alive, "1h");
    }

    #[test]
    fn linux_cuda_workstation_uses_high_ram_accel_profile() {
        let defaults = defaults_for(&snapshot(64, GpuKind::Cuda));
        assert_eq!(defaults.bucket, ProfileBucket::HighRamAccel);
        assert_eq!(defaults.ctx, 65_536);
    }

    #[test]
    fn mid_ram_with_no_gpu_picks_conservative_defaults() {
        let defaults = defaults_for(&snapshot(24, GpuKind::None));
        assert_eq!(defaults.bucket, ProfileBucket::MidRamNoGpu);
        assert_eq!(defaults.ctx, 16_384);
        assert_eq!(defaults.keep_alive, "10m");
    }

    #[test]
    fn unknown_ram_falls_back_to_low_ram() {
        let mut snap = snapshot(0, GpuKind::None);
        snap.ram.total_bytes = None;
        let defaults = defaults_for(&snap);
        assert_eq!(defaults.bucket, ProfileBucket::LowRam);
    }
}
