//! Process lifecycle management: kill trees, PID files, orphan cleanup, child
//! enumeration. Shared by all agent backends that spawn subprocesses.

use std::collections::HashSet;
use std::path::Path;

/// Kill an entire process group by pgid (== the agent's pid). No-op for
/// pgid <= 1. On Unix this signals the whole group; on Windows it enumerates
/// the process tree and terminates each process individually.
pub fn kill_process_group(pgid: i32) {
    if pgid <= 1 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
        libc::kill(-pgid, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        kill_tree(pgid);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = pgid;
}

/// Kill every process belonging to session `sid` (== the agent's pid, since the
/// child was spawned via `setsid`). Enumerates live pids and signals the matches
/// (SIGTERM then SIGKILL). No-op for `sid <= 1`.
///
/// macOS implementation uses `proc_listpids`; on Windows this falls back to
/// `kill_process_group`; other platforms are no-ops.
#[cfg(target_os = "macos")]
pub fn kill_session(sid: i32) {
    if sid <= 1 {
        return;
    }
    const PROC_ALL_PIDS: u32 = 1;
    unsafe {
        let cap = libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0);
        if cap <= 0 {
            return;
        }
        let mut pids = vec![0i32; cap as usize / std::mem::size_of::<i32>() + 16];
        let bytes = libc::proc_listpids(
            PROC_ALL_PIDS,
            0,
            pids.as_mut_ptr() as *mut libc::c_void,
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        );
        if bytes <= 0 {
            return;
        }
        let n = bytes as usize / std::mem::size_of::<i32>();
        for &pid in &pids[..n] {
            if pid <= 1 {
                continue;
            }
            if libc::getsid(pid) == sid {
                libc::kill(pid, libc::SIGTERM);
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn kill_session(sid: i32) {
    #[cfg(windows)]
    kill_process_group(sid);
    #[cfg(not(windows))]
    let _ = sid;
}

/// Write the agent's PID to a file so it can be reaped on next startup.
pub fn write_pid_file(path: &Path, pid: u32) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(path, pid.to_string()) {
        tracing::warn!(error=%e, path=%path.display(), "failed to write agent PID file");
    }
}

/// Kill any orphaned agent process recorded in `pid_file`, then remove the
/// file. Call this before spawning a new daemon to ensure no stale agent tree
/// survives a crash or forced kill.
pub fn cleanup_orphaned_agent(pid_file: &Path) {
    let content = match std::fs::read_to_string(pid_file) {
        Ok(c) => c,
        Err(_) => return,
    };
    let pid: i32 = match content.trim().parse() {
        Ok(p) if p > 1 => p,
        _ => {
            let _ = std::fs::remove_file(pid_file);
            return;
        }
    };
    if process_is_alive(pid) {
        tracing::info!(pid, "killing orphaned agent process tree");
        kill_session(pid);
        kill_process_group(pid);
    }
    let _ = std::fs::remove_file(pid_file);
}

/// Check whether a process with the given PID is currently alive.
pub fn process_is_alive(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    #[cfg(unix)]
    {
        (unsafe { libc::kill(pid, 0) } == 0)
    }
    #[cfg(windows)]
    {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
        let mut s = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        );
        s.refresh_processes(
            ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid as u32)]),
            true,
        );
        s.process(sysinfo::Pid::from_u32(pid as u32)).is_some()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// List all direct child PIDs of `parent_pid`.
///
/// macOS uses `proc_listpids(PROC_PPID_ONLY)` (single syscall);
/// Windows/Linux use `sysinfo` to enumerate the process tree.
#[cfg(target_os = "macos")]
pub fn list_child_pids(parent_pid: i32) -> HashSet<i32> {
    let mut children = HashSet::new();
    if parent_pid <= 1 {
        return children;
    }
    const PROC_PPID_ONLY: u32 = 6;
    unsafe {
        let cap = libc::proc_listpids(PROC_PPID_ONLY, parent_pid as u32, std::ptr::null_mut(), 0);
        if cap <= 0 {
            return children;
        }
        let mut pids = vec![0i32; cap as usize / std::mem::size_of::<i32>() + 16];
        let bytes = libc::proc_listpids(
            PROC_PPID_ONLY,
            parent_pid as u32,
            pids.as_mut_ptr() as *mut libc::c_void,
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        );
        if bytes <= 0 {
            return children;
        }
        let n = bytes as usize / std::mem::size_of::<i32>();
        for &pid in &pids[..n] {
            if pid > 1 {
                children.insert(pid);
            }
        }
    }
    children
}

#[cfg(not(target_os = "macos"))]
pub fn list_child_pids(parent_pid: i32) -> HashSet<i32> {
    #[allow(unused_mut)]
    let mut children = HashSet::new();
    if parent_pid <= 1 {
        return children;
    }
    #[cfg(windows)]
    {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
        let mut s = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        );
        s.refresh_processes(ProcessesToUpdate::All, true);
        let parent = sysinfo::Pid::from_u32(parent_pid as u32);
        for (pid, proc_) in s.processes() {
            if proc_.parent() == Some(parent) {
                children.insert(pid.as_u32() as i32);
            }
        }
    }
    #[cfg(not(windows))]
    let _ = parent_pid;
    children
}

/// Return child PIDs of `parent_pid` that were NOT in the `before` snapshot.
pub fn find_new_child_pids(parent_pid: i32, before: &HashSet<i32>) -> Vec<i32> {
    let current = list_child_pids(parent_pid);
    current.difference(before).copied().collect()
}

/// Kill a single process by PID (cross-platform).
pub fn kill_single_process(pid: u32) {
    if pid <= 1 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
        let spid = sysinfo::Pid::from_u32(pid);
        let mut s = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        );
        s.refresh_processes(ProcessesToUpdate::Some(&[spid]), true);
        if let Some(p) = s.process(spid) {
            p.kill();
        }
    }
}

#[cfg(windows)]
fn kill_tree(root_pid: i32) {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
    let mut s = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    s.refresh_processes(ProcessesToUpdate::All, true);
    let root = sysinfo::Pid::from_u32(root_pid as u32);
    let mut to_kill = vec![root];
    let mut killed = HashSet::new();
    while let Some(pid) = to_kill.pop() {
        if !killed.insert(pid) {
            continue;
        }
        for (child_pid, proc_) in s.processes() {
            if proc_.parent() == Some(pid) {
                to_kill.push(*child_pid);
            }
        }
        if let Some(p) = s.process(pid) {
            p.kill();
        }
    }
}
