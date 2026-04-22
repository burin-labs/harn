use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;

pub struct OrchestratorProcessTestLock {
    file: File,
}

impl Drop for OrchestratorProcessTestLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn lock_orchestrator_process_tests() -> OrchestratorProcessTestLock {
    let path = orchestrator_process_lock_path();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap_or_else(|error| panic!("failed to open {}: {error}", path.display()));
    file.lock_exclusive()
        .unwrap_or_else(|error| panic!("failed to lock {}: {error}", path.display()));
    OrchestratorProcessTestLock { file }
}

fn orchestrator_process_lock_path() -> PathBuf {
    std::env::temp_dir().join("harn-orchestrator-process-tests.lock")
}

#[allow(dead_code)]
pub fn wait_for_readyz(child: &mut Child, base_url: &str, timeout: Duration) -> Result<(), String> {
    let Some(target) = ReadyzTarget::parse(base_url)? else {
        return Ok(());
    };
    let deadline = Instant::now() + timeout;
    let mut last_error = None;

    while Instant::now() < deadline {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("failed to inspect orchestrator process: {error}"))?
        {
            return Err(format!(
                "orchestrator exited before readiness probe succeeded: {status}"
            ));
        }

        match probe_readyz(&target) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(Duration::from_millis(25));
    }

    Err(format!(
        "timed out waiting for orchestrator readiness at {base_url}/readyz: {}",
        last_error.unwrap_or_else(|| "no probe attempt completed".to_string())
    ))
}

struct ReadyzTarget {
    host: String,
    port: u16,
}

impl ReadyzTarget {
    fn parse(base_url: &str) -> Result<Option<Self>, String> {
        let url = url::Url::parse(base_url)
            .map_err(|error| format!("invalid listener URL `{base_url}`: {error}"))?;
        if url.scheme() == "https" {
            return Ok(None);
        }
        if url.scheme() != "http" {
            return Err(format!("unsupported listener URL scheme: `{base_url}`"));
        }
        let host = url
            .host_str()
            .ok_or_else(|| format!("listener URL has no host: `{base_url}`"))?
            .to_string();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| format!("listener URL has no port: `{base_url}`"))?;
        Ok(Some(Self { host, port }))
    }
}

fn probe_readyz(target: &ReadyzTarget) -> Result<(), String> {
    let addr = format!("{}:{}", target.host, target.port);
    let mut stream = TcpStream::connect_timeout(
        &addr
            .parse()
            .map_err(|error| format!("invalid readiness address `{addr}`: {error}"))?,
        Duration::from_millis(250),
    )
    .map_err(|error| format!("readiness connect failed: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .map_err(|error| format!("failed to set readiness read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_millis(250)))
        .map_err(|error| format!("failed to set readiness write timeout: {error}"))?;
    write!(
        stream,
        "GET /readyz HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
        target.host, target.port
    )
    .map_err(|error| format!("failed to write readiness request: {error}"))?;

    let mut response = [0_u8; 128];
    let read = stream
        .read(&mut response)
        .map_err(|error| format!("failed to read readiness response: {error}"))?;
    let response = std::str::from_utf8(&response[..read])
        .map_err(|error| format!("readiness response was not UTF-8: {error}"))?;
    if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
        Ok(())
    } else {
        Err(format!(
            "readiness probe returned {}",
            response.lines().next().unwrap_or("<empty response>")
        ))
    }
}
