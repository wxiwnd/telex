//! Process ancestry helpers for session handoff.
//!
//! Hooks run as children of the harness process. The CLI runs as a child of
//! the same harness process, usually through a shell. Walking the parent chain
//! lets the login command find the session id written by an earlier hook.

use std::process::Command;

pub fn ancestor_pids() -> Vec<u32> {
    let mut out = Vec::new();
    // `getppid` is the only ancestor available without spawning a helper
    // process; the rest of the chain comes from /proc or ps.
    // SAFETY: getppid has no preconditions and cannot fail.
    let mut pid = unsafe { libc::getppid() } as u32;
    while pid > 1 && !out.contains(&pid) && out.len() < 16 {
        out.push(pid);
        let Some(parent) = parent_pid(pid) else {
            break;
        };
        pid = parent;
    }
    out
}

#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .and_then(|value| value.trim().parse().ok())
}

#[cfg(not(target_os = "linux"))]
fn parent_pid(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}
