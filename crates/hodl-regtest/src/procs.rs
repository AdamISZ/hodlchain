//! Process management: PID files, alive checks, graceful kill.

use anyhow::{Context, Result};
use std::path::Path;
use std::time::{Duration, Instant};

pub fn write_pid(path: &Path, pid: u32) -> Result<()> {
    std::fs::write(path, format!("{pid}\n"))
        .with_context(|| format!("write {}", path.display()))
}

pub fn read_pid(path: &Path) -> Result<Option<u32>> {
    if !path.exists() {
        return Ok(None);
    }
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(s.trim().parse::<u32>().ok())
}

/// Check liveness via `kill(pid, 0)`. Returns false if the process
/// does not exist OR if we lack permission to signal it (unlikely for
/// a same-user process).
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: trivially safe — kill(0) is a no-op signal used only
    // for existence checking and does not mutate process state.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// SIGTERM, wait up to `grace`, then SIGKILL. Returns once the
/// process is gone (or never existed).
pub fn graceful_kill(pid: u32, grace: Duration) -> Result<()> {
    if !pid_alive(pid) {
        return Ok(());
    }
    // SAFETY: same as above; we own the PID we're signalling.
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Still alive after grace — escalate.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    // Brief wait so the caller can observe a clean exit.
    for _ in 0..20 {
        if !pid_alive(pid) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("pid {pid} did not exit even after SIGKILL");
}
