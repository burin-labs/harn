use std::time::{Duration, Instant};

/// An operating-system birth boundary anchored to the caller's monotonic clock.
/// Wall-clock changes after observation do not move this boundary. The native
/// clock's resolution is retained rather than implying nanosecond precision.
#[derive(Clone, Copy, Debug)]
pub struct ProcessStart {
    pub instant: Instant,
    pub resolution: Duration,
}

/// Observe the real process birth, including work before the entry point.
/// Unsupported or inaccessible measurements remain absent.
pub fn process_start(pid: u32) -> Option<ProcessStart> {
    let observed = Instant::now();
    let (age, resolution) = native_age(pid)?;
    Some(ProcessStart {
        instant: observed.checked_sub(age)?,
        resolution,
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn native_age(pid: u32) -> Option<(Duration, Duration)> {
    let birth_ticks = super::process_identity(pid)?;
    let ticks_per_second = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok()?;
    if ticks_per_second == 0 {
        return None;
    }
    let mut uptime = std::mem::MaybeUninit::<libc::timespec>::uninit();
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, uptime.as_mut_ptr()) } != 0 {
        return None;
    }
    let uptime = unsafe { uptime.assume_init() };
    let uptime = Duration::new(
        u64::try_from(uptime.tv_sec).ok()?,
        u32::try_from(uptime.tv_nsec).ok()?,
    );
    let birth = Duration::new(
        birth_ticks / ticks_per_second,
        u32::try_from((birth_ticks % ticks_per_second) * 1_000_000_000 / ticks_per_second).ok()?,
    );
    Some((
        uptime.checked_sub(birth)?,
        Duration::from_nanos(1_000_000_000_u64.div_ceil(ticks_per_second)),
    ))
}

#[cfg(any(target_os = "macos", target_os = "ios", windows))]
fn native_age(pid: u32) -> Option<(Duration, Duration)> {
    let birth = super::process_identity(pid)?;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let (birth, resolution) = (Duration::from_micros(birth), Duration::from_micros(1));
    #[cfg(windows)]
    let (birth, resolution) = {
        // FILETIME is measured in 100 ns since 1601; SystemTime uses 1970.
        let ticks = birth.checked_sub(116_444_736_000_000_000)?;
        (
            Duration::new(ticks / 10_000_000, ((ticks % 10_000_000) * 100) as u32),
            Duration::from_nanos(100),
        )
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some((now.checked_sub(birth)?, resolution))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    windows
)))]
fn native_age(_pid: u32) -> Option<(Duration, Duration)> {
    None
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        windows
    )
))]
mod tests {
    use super::*;

    #[test]
    fn birth_precedes_the_measurement_instead_of_restarting_its_clock() {
        let before = Instant::now();
        let start = process_start(std::process::id()).expect("native process birth");
        assert!(start.instant < before);
        assert!(!start.resolution.is_zero());
        assert!(process_start(u32::MAX).is_none());
    }
}
