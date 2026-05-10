//! WASI-mediated subprocess sandbox for testbench mode.
//!
//! `WasiToolchain` points to a directory of pre-compiled WASM modules.
//! When [`wasi_module_for`] finds `<toolchain_dir>/<program>.wasm`,
//! [`run_wasm_module`] runs that module under wasmtime with:
//!
//! - **Virtual clocks**: `clock_time_get` returns the testbench
//!   `MockClock`'s current time. `poll_oneoff` clock subscriptions
//!   advance the testbench clock by the requested duration instead of
//!   blocking the host thread.
//! - **Filesystem overlay**: writes to the WASM module's preopened
//!   directory are merged back into the active overlay after exit so
//!   the parent observes them in the standard overlay diff.
//! - **Network**: wasmtime-wasi's socket imports are not linked — any
//!   WASM socket call fails at link time with a deterministic error,
//!   matching the testbench's deny-by-default network policy.
//!
//! ## Limitations
//!
//! - Only WASI preview 1 modules (`wasm32-wasi` target) are supported.
//! - `poll_oneoff` FD-read/write subscriptions return `ERRNO_NOTSUP` —
//!   tools that block on stdin/stdout polling are not supported. Use
//!   args, env, or files for input.
//! - The preopened directory starts empty; reads of files that exist
//!   only in the overlay are not pre-staged. Tools that need overlay
//!   reads should be invoked after the overlay's relevant files have
//!   been materialized.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wasmtime::{Caller, Engine, Linker, Module, Store};
use wasmtime_wasi::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::preview1::{add_to_linker_sync, WasiP1Ctx};
use wasmtime_wasi::{
    DirPerms, FilePerms, HostMonotonicClock, HostWallClock, I32Exit, WasiCtxBuilder,
};

use crate::testbench::overlay_fs::{active_overlay, OverlayFs};

// ── WASI preview 1 ABI memory layout ─────────────────────────────────────────

/// Subscription record size (`__wasi_subscription_t`).
const SUB_SIZE: usize = 48;
const SUB_USERDATA_OFFSET: usize = 0;
const SUB_EVENTTYPE_OFFSET: usize = 8;
const SUB_CLOCK_ID_OFFSET: usize = 16;
const SUB_CLOCK_TIMEOUT_OFFSET: usize = 24;
const SUB_CLOCK_FLAGS_OFFSET: usize = 40;

/// Event record size (`__wasi_event_t`).
const EVT_SIZE: usize = 32;
const EVT_USERDATA_OFFSET: usize = 0;
const EVT_ERRNO_OFFSET: usize = 8;
const EVT_TYPE_OFFSET: usize = 10;

const EVENTTYPE_CLOCK: u8 = 0;
const CLOCKID_REALTIME: u32 = 0;
const CLOCK_ABSTIME_FLAG: u16 = 1;

const ERRNO_SUCCESS: i32 = 0;
const ERRNO_INVAL: i32 = 28;
const ERRNO_NOTSUP: i32 = 58;

// ── Custom clock implementations ─────────────────────────────────────────────

struct TestbenchWallClock;

impl HostWallClock for TestbenchWallClock {
    fn resolution(&self) -> Duration {
        Duration::from_millis(1)
    }
    fn now(&self) -> Duration {
        let ms = crate::clock_mock::now_ms().max(0) as u64;
        Duration::from_millis(ms)
    }
}

struct TestbenchMonotonicClock;

impl HostMonotonicClock for TestbenchMonotonicClock {
    fn resolution(&self) -> u64 {
        1_000_000
    }
    fn now(&self) -> u64 {
        (crate::clock_mock::instant_now().as_millis() as u64).saturating_mul(1_000_000)
    }
}

// ── Active toolchain thread-local ────────────────────────────────────────────

thread_local! {
    static ACTIVE_WASI_TOOLCHAIN: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[must_use = "the toolchain detaches on drop; bind the guard to a `_guard` local"]
pub struct WasiToolchainGuard {
    previous: Option<PathBuf>,
}

impl Drop for WasiToolchainGuard {
    fn drop(&mut self) {
        let prev = self.previous.take();
        ACTIVE_WASI_TOOLCHAIN.with(|slot| {
            *slot.borrow_mut() = prev;
        });
    }
}

/// Install `dir` as the active WASI toolchain. Subsequent `intercept_spawn`
/// calls on the same thread will look up `<dir>/<program>.wasm` and run the
/// module under wasmtime instead of spawning a native subprocess.
pub fn install_wasi_toolchain(dir: PathBuf) -> WasiToolchainGuard {
    let previous = ACTIVE_WASI_TOOLCHAIN.with(|slot| slot.replace(Some(dir)));
    WasiToolchainGuard { previous }
}

/// Currently-installed WASI toolchain directory, if any.
pub fn active_wasi_toolchain() -> Option<PathBuf> {
    ACTIVE_WASI_TOOLCHAIN.with(|slot| slot.borrow().clone())
}

/// Look up `<active_toolchain>/<program>.wasm`. Returns the WASM path if
/// the file exists; `None` to fall through to the native spawn path.
pub fn wasi_module_for(program: &str) -> Option<PathBuf> {
    let toolchain = active_wasi_toolchain()?;
    let wasm_path = toolchain.join(format!("{program}.wasm"));
    if wasm_path.exists() {
        Some(wasm_path)
    } else {
        None
    }
}

// ── WASM execution ───────────────────────────────────────────────────────────

/// Output of a WASM module run.
#[derive(Debug, Clone)]
pub struct WasiOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    /// Virtual milliseconds elapsed on the testbench monotonic clock during
    /// this run — captures both `clock_time_get` reads and `poll_oneoff`
    /// sleep advances.
    pub virtual_duration_ms: u64,
}

/// Run the WASM module at `wasm_path` under the active testbench context.
///
/// Stdout/stderr are captured. The module's preopened directory is a fresh
/// temp dir; any files it writes are merged into the active overlay (if
/// installed) before this function returns. The active `MockClock` drives
/// every WASI clock read and every `poll_oneoff` clock subscription.
pub fn run_wasm_module(
    wasm_path: &Path,
    args: &[String],
    env: &[(String, String)],
) -> Result<WasiOutput, String> {
    let started_mono_ms = crate::clock_mock::instant_now().as_millis() as u64;

    let wasm_bytes =
        std::fs::read(wasm_path).map_err(|e| format!("read {}: {e}", wasm_path.display()))?;

    let engine = build_engine()?;
    let module = Module::new(&engine, &wasm_bytes)
        .map_err(|e| format!("compile {}: {e}", wasm_path.display()))?;

    let stdout_pipe = MemoryOutputPipe::new(8 * 1024 * 1024);
    let stderr_pipe = MemoryOutputPipe::new(1024 * 1024);
    let stdout_reader = stdout_pipe.clone();
    let stderr_reader = stderr_pipe.clone();

    let tmpdir = tempfile::TempDir::new().map_err(|e| format!("create temp dir: {e}"))?;

    let mut builder = WasiCtxBuilder::new();
    builder
        .wall_clock(TestbenchWallClock)
        .monotonic_clock(TestbenchMonotonicClock)
        .stdout(stdout_pipe)
        .stderr(stderr_pipe)
        .stdin(MemoryInputPipe::new(bytes::Bytes::new()));

    let program_name = wasm_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "wasm".to_string());
    let mut all_args = Vec::with_capacity(args.len() + 1);
    all_args.push(program_name);
    all_args.extend(args.iter().cloned());
    builder.args(&all_args);

    for (key, val) in env {
        builder.env(key, val);
    }

    builder
        .preopened_dir(tmpdir.path(), "/", DirPerms::all(), FilePerms::all())
        .map_err(|e| format!("preopen dir: {e}"))?;

    let ctx = builder.build_p1();
    let mut store = Store::new(&engine, ctx);

    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    add_to_linker_sync(&mut linker, |ctx| ctx).map_err(|e| format!("add WASI to linker: {e}"))?;

    install_clock_overrides(&mut linker)?;

    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("instantiate {}: {e}", wasm_path.display()))?;

    let start = instance
        .get_func(&mut store, "_start")
        .ok_or_else(|| format!("module {} has no _start export", wasm_path.display()))?;

    let exit_code = match start.call(&mut store, &[], &mut []) {
        Ok(()) => 0,
        Err(err) => {
            if let Some(exit) = err.downcast_ref::<I32Exit>() {
                exit.0
            } else {
                let mut sink = stderr_reader.clone();
                push_trap_to_stderr(&mut sink, &err);
                1
            }
        }
    };

    if let Some(overlay) = active_overlay() {
        sync_tmpdir_to_overlay(tmpdir.path(), &overlay)?;
    }

    let ended_mono_ms = crate::clock_mock::instant_now().as_millis() as u64;

    Ok(WasiOutput {
        stdout: stdout_reader.contents().to_vec(),
        stderr: stderr_reader.contents().to_vec(),
        exit_code,
        virtual_duration_ms: ended_mono_ms.saturating_sub(started_mono_ms),
    })
}

fn build_engine() -> Result<Engine, String> {
    let mut config = wasmtime::Config::new();
    config.async_support(false);
    Engine::new(&config).map_err(|e| format!("build wasmtime engine: {e}"))
}

/// Replace the standard `clock_time_get` and `poll_oneoff` exports with
/// virtualized versions backed by the testbench mock clock.
fn install_clock_overrides(linker: &mut Linker<WasiP1Ctx>) -> Result<(), String> {
    linker.allow_shadowing(true);

    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            wasi_clock_time_get,
        )
        .map_err(|e| format!("shadow clock_time_get: {e}"))?;

    linker
        .func_wrap("wasi_snapshot_preview1", "poll_oneoff", wasi_poll_oneoff)
        .map_err(|e| format!("shadow poll_oneoff: {e}"))?;

    Ok(())
}

fn wasi_clock_time_get(
    mut caller: Caller<'_, WasiP1Ctx>,
    clock_id: i32,
    _precision: i64,
    result_ptr: i32,
) -> i32 {
    let nanos: u64 = match clock_id {
        x if x as u32 == CLOCKID_REALTIME => {
            (crate::clock_mock::now_ms().max(0) as u64).saturating_mul(1_000_000)
        }
        1 => (crate::clock_mock::instant_now().as_millis() as u64).saturating_mul(1_000_000),
        _ => return ERRNO_INVAL,
    };
    let mem = match caller.get_export("memory") {
        Some(wasmtime::Extern::Memory(m)) => m,
        _ => return ERRNO_INVAL,
    };
    let bytes = nanos.to_le_bytes();
    let data = mem.data_mut(&mut caller);
    let ptr = result_ptr as usize;
    if ptr.checked_add(8).is_none_or(|end| end > data.len()) {
        return ERRNO_INVAL;
    }
    data[ptr..ptr + 8].copy_from_slice(&bytes);
    ERRNO_SUCCESS
}

/// Custom `poll_oneoff` implementation that fast-forwards CLOCK
/// subscriptions on the testbench mock clock instead of blocking the host
/// thread. FD-read/write subscriptions return `ERRNO_NOTSUP` — tools that
/// need to poll file descriptors are out of scope for testbench WASI mode.
fn wasi_poll_oneoff(
    mut caller: Caller<'_, WasiP1Ctx>,
    in_ptr: i32,
    out_ptr: i32,
    nsubscriptions: i32,
    nevents_ptr: i32,
) -> i32 {
    if nsubscriptions <= 0 {
        return ERRNO_INVAL;
    }
    let mem = match caller.get_export("memory") {
        Some(wasmtime::Extern::Memory(m)) => m,
        _ => return ERRNO_INVAL,
    };
    let mem_len = mem.data_size(&caller);

    let in_base = in_ptr as usize;
    let out_base = out_ptr as usize;
    let nsubs = nsubscriptions as usize;

    if !range_in_bounds(in_base, SUB_SIZE, nsubs, mem_len)
        || !range_in_bounds(out_base, EVT_SIZE, nsubs, mem_len)
    {
        return ERRNO_INVAL;
    }

    let mut nevents: u32 = 0;
    for i in 0..nsubs {
        let sub_base = in_base + i * SUB_SIZE;
        let (userdata, eventtype, clock_id, timeout_ns, flags) = {
            let data = mem.data(&caller);
            let userdata = u64::from_le_bytes(
                data[sub_base + SUB_USERDATA_OFFSET..sub_base + SUB_USERDATA_OFFSET + 8]
                    .try_into()
                    .unwrap(),
            );
            let eventtype = data[sub_base + SUB_EVENTTYPE_OFFSET];
            let clock_id = u32::from_le_bytes(
                data[sub_base + SUB_CLOCK_ID_OFFSET..sub_base + SUB_CLOCK_ID_OFFSET + 4]
                    .try_into()
                    .unwrap(),
            );
            let timeout_ns = u64::from_le_bytes(
                data[sub_base + SUB_CLOCK_TIMEOUT_OFFSET..sub_base + SUB_CLOCK_TIMEOUT_OFFSET + 8]
                    .try_into()
                    .unwrap(),
            );
            let flags = u16::from_le_bytes(
                data[sub_base + SUB_CLOCK_FLAGS_OFFSET..sub_base + SUB_CLOCK_FLAGS_OFFSET + 2]
                    .try_into()
                    .unwrap(),
            );
            (userdata, eventtype, clock_id, timeout_ns, flags)
        };

        if eventtype != EVENTTYPE_CLOCK {
            return ERRNO_NOTSUP;
        }

        let is_absolute = flags & CLOCK_ABSTIME_FLAG != 0;
        if is_absolute {
            let now_ns = if clock_id == CLOCKID_REALTIME {
                (crate::clock_mock::now_ms().max(0) as u64).saturating_mul(1_000_000)
            } else {
                (crate::clock_mock::instant_now().as_millis() as u64).saturating_mul(1_000_000)
            };
            if timeout_ns > now_ns {
                crate::clock_mock::advance(Duration::from_nanos(timeout_ns - now_ns));
            }
        } else if timeout_ns > 0 {
            crate::clock_mock::advance(Duration::from_nanos(timeout_ns));
        }

        let evt_base = out_base + (nevents as usize) * EVT_SIZE;
        {
            let data_mut = mem.data_mut(&mut caller);
            data_mut[evt_base + EVT_USERDATA_OFFSET..evt_base + EVT_USERDATA_OFFSET + 8]
                .copy_from_slice(&userdata.to_le_bytes());
            data_mut[evt_base + EVT_ERRNO_OFFSET..evt_base + EVT_ERRNO_OFFSET + 2]
                .copy_from_slice(&0u16.to_le_bytes());
            data_mut[evt_base + EVT_TYPE_OFFSET] = EVENTTYPE_CLOCK;
            for b in &mut data_mut[evt_base + EVT_TYPE_OFFSET + 1..evt_base + EVT_SIZE] {
                *b = 0;
            }
        }
        nevents += 1;
    }

    let nev_ptr = nevents_ptr as usize;
    if nev_ptr.checked_add(4).is_none_or(|end| end > mem_len) {
        return ERRNO_INVAL;
    }
    let data_mut = mem.data_mut(&mut caller);
    data_mut[nev_ptr..nev_ptr + 4].copy_from_slice(&nevents.to_le_bytes());

    ERRNO_SUCCESS
}

/// Whether `[base, base + stride * count)` is in `[0, mem_len)`.
fn range_in_bounds(base: usize, stride: usize, count: usize, mem_len: usize) -> bool {
    stride
        .checked_mul(count)
        .and_then(|len| base.checked_add(len))
        .map(|end| end <= mem_len)
        .unwrap_or(false)
}

fn push_trap_to_stderr(stderr: &mut MemoryOutputPipe, err: &wasmtime::Error) {
    use wasmtime_wasi::HostOutputStream;
    let msg = format!("wasi trap: {err}\n");
    let _ = stderr.write(bytes::Bytes::from(msg.into_bytes()));
}

/// Mirror files written by the WASM module into the overlay so the
/// parent observes them in the standard diff. Relative tmpdir paths are
/// rebased onto the overlay root — `overlay.write` only stores entries
/// whose canonicalized path starts with that root.
fn sync_tmpdir_to_overlay(tmpdir: &Path, overlay: &OverlayFs) -> Result<(), String> {
    let root = overlay.root().to_path_buf();
    for entry in walkdir::WalkDir::new(tmpdir).into_iter().flatten() {
        if entry.file_type().is_file() {
            let abs = entry.path();
            let rel = abs
                .strip_prefix(tmpdir)
                .map_err(|e| format!("strip prefix: {e}"))?;
            let contents =
                std::fs::read(abs).map_err(|e| format!("read {}: {e}", abs.display()))?;
            let target = root.join(rel);
            overlay
                .write(&target, &contents)
                .map_err(|e| format!("overlay write {}: {e}", target.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock_mock;

    /// Minimal WASI module: read realtime clock, write the 8-byte little-
    /// endian nanosecond value to stdout, exit 0.
    const CLOCK_READ_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "clock_time_get"
            (func $clock_time_get (param i32 i64 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "proc_exit"
            (func $proc_exit (param i32)))
          (memory (export "memory") 1)
          ;; iovec at offset 16: { ptr=64, len=8 }
          (data (i32.const 16) "\40\00\00\00\08\00\00\00")
          (func (export "_start")
            ;; clock_time_get(REALTIME, precision=1, result=64)
            i32.const 0
            i64.const 1
            i32.const 64
            call $clock_time_get
            drop
            ;; fd_write(stdout=1, iovec=16, iovec_len=1, nwritten=80)
            i32.const 1
            i32.const 16
            i32.const 1
            i32.const 80
            call $fd_write
            drop
            i32.const 0
            call $proc_exit
          )
        )
    "#;

    /// Sleeps for 5 seconds via a single CLOCK_MONOTONIC relative
    /// `poll_oneoff` subscription, then exits 0.
    const SLEEP_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "poll_oneoff"
            (func $poll_oneoff (param i32 i32 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "proc_exit"
            (func $proc_exit (param i32)))
          (memory (export "memory") 1)
          (func (export "_start")
            ;; subscription at memory offset 0:
            ;;   userdata=0xbeef, type=0 (CLOCK), id=1 (MONOTONIC),
            ;;   timeout=5_000_000_000 ns, flags=0 (relative)
            i32.const 0
            i64.const 0xbeef
            i64.store
            i32.const 8
            i32.const 0
            i32.store8
            i32.const 16
            i32.const 1
            i32.store
            i32.const 24
            i64.const 5000000000
            i64.store
            i32.const 40
            i32.const 0
            i32.store16

            ;; poll_oneoff(in=0, out=64, nsubs=1, nevents_ptr=128)
            i32.const 0
            i32.const 64
            i32.const 1
            i32.const 128
            call $poll_oneoff
            drop

            i32.const 0
            call $proc_exit
          )
        )
    "#;

    /// Writes "hello\n" to /out.txt and exits with the path_open errno on
    /// failure (or 0 on success).
    const WRITE_FILE_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "path_open"
            (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_close"
            (func $fd_close (param i32) (result i32)))
          (import "wasi_snapshot_preview1" "proc_exit"
            (func $proc_exit (param i32)))
          (memory (export "memory") 1)
          ;; path "out.txt" at offset 0
          (data (i32.const 0) "out.txt")
          ;; iovec at 32: ptr=64, len=6
          (data (i32.const 32) "\40\00\00\00\06\00\00\00")
          ;; payload "hello\n" at offset 64
          (data (i32.const 64) "hello\n")
          (func (export "_start") (local $err i32) (local $fd i32)
            ;; path_open(
            ;;   fd=3 (preopened "/"), dirflags=0, path=0, path_len=7,
            ;;   oflags=0x9 (CREAT|TRUNC), rights_base=0x40 (FD_WRITE),
            ;;   rights_inheriting=0, fdflags=0, opened_fd_ptr=80)
            i32.const 3
            i32.const 0
            i32.const 0
            i32.const 7
            i32.const 9
            i64.const 0x40
            i64.const 0
            i32.const 0
            i32.const 80
            call $path_open
            local.tee $err
            i32.const 0
            i32.ne
            if
              ;; nonzero errno; surface it as exit_code = 100 + errno
              local.get $err
              i32.const 100
              i32.add
              call $proc_exit
            end

            ;; opened_fd from memory[80]
            i32.const 80
            i32.load
            local.set $fd

            ;; fd_write(fd, iovec=32, iovec_len=1, nwritten=88)
            local.get $fd
            i32.const 32
            i32.const 1
            i32.const 88
            call $fd_write
            local.tee $err
            i32.const 0
            i32.ne
            if
              local.get $err
              i32.const 200
              i32.add
              call $proc_exit
            end

            ;; fd_close(fd)
            local.get $fd
            call $fd_close
            drop
            i32.const 0
            call $proc_exit
          )
        )
    "#;

    fn compile_wat(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("WAT parse failed")
    }

    fn write_temp_wasm(wat: &str) -> (tempfile::TempDir, PathBuf) {
        let bytes = compile_wat(wat);
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("module.wasm");
        std::fs::write(&path, &bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn clock_time_get_returns_testbench_time() {
        let start_ms: i64 = 1_767_225_600_000;
        let _guard = clock_mock::install_override(clock_mock::MockClock::at_wall_ms(start_ms));

        let (_dir, path) = write_temp_wasm(CLOCK_READ_WAT);
        let output = run_wasm_module(&path, &[], &[]).expect("run wasm");
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout.len(), 8);
        let nanos = u64::from_le_bytes(output.stdout[..8].try_into().unwrap());
        assert_eq!(nanos, start_ms as u64 * 1_000_000);
    }

    #[test]
    fn poll_oneoff_sleep_advances_testbench_clock_without_blocking() {
        let start_ms: i64 = 1_767_225_600_000;
        let _guard = clock_mock::install_override(clock_mock::MockClock::at_wall_ms(start_ms));

        let (_dir, path) = write_temp_wasm(SLEEP_WAT);
        let wall_before = std::time::Instant::now();
        let output = run_wasm_module(&path, &[], &[]).expect("run wasm");
        let wall_elapsed = wall_before.elapsed();

        assert_eq!(output.exit_code, 0);
        assert_eq!(
            output.virtual_duration_ms, 5000,
            "virtual clock should advance by sleep duration"
        );
        assert!(
            wall_elapsed < std::time::Duration::from_secs(1),
            "wall clock should not actually sleep 5s, took {wall_elapsed:?}"
        );
    }

    #[test]
    fn writes_route_through_active_overlay() {
        use crate::testbench::overlay_fs::{install_overlay, OverlayFs};
        use std::sync::Arc;

        let workdir = tempfile::TempDir::new().unwrap();
        let overlay = Arc::new(OverlayFs::rooted_at(workdir.path()));
        let _ofs = install_overlay(Arc::clone(&overlay));

        let (_dir, path) = write_temp_wasm(WRITE_FILE_WAT);
        let output = run_wasm_module(&path, &[], &[]).expect("run wasm");
        assert_eq!(
            output.exit_code,
            0,
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let diff = overlay.diff();
        assert_eq!(diff.len(), 1, "expected one overlay write, got {:?}", diff);
        assert!(
            diff[0].path.ends_with("out.txt"),
            "diff path should end with out.txt, got {:?}",
            diff[0].path
        );
        let bytes = overlay.read(&workdir.path().join("out.txt")).expect("read");
        assert_eq!(&bytes, b"hello\n");
    }

    #[test]
    fn missing_module_falls_through_to_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let _guard = install_wasi_toolchain(dir.path().to_path_buf());
        assert!(wasi_module_for("nonexistent").is_none());
    }

    #[test]
    fn module_present_resolves_to_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let stub = dir.path().join("hello.wasm");
        std::fs::write(&stub, b"\0asm").unwrap();
        let _guard = install_wasi_toolchain(dir.path().to_path_buf());
        let resolved = wasi_module_for("hello").expect("module resolves");
        assert_eq!(resolved, stub);
    }
}
