//! Suspend / resume a stage's process tree (work/build/PAUSE with `--suspend-running`; tools/build/pause.py does the
//! same from outside). Windows: NtSuspendProcess / NtResumeProcess on every process of the tree.
use std::collections::{BTreeMap, BTreeSet};
use sysinfo::System;

/// every pid of the tree under `root` (root included), parents before children
pub fn tree(sys: &System, root: u32) -> Vec<u32> {
    let mut kids: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (pid, p) in sys.processes() {
        if let Some(pp) = p.parent() {
            kids.entry(pp.as_u32()).or_default().push(pid.as_u32());
        }
    }
    let mut out = vec![];
    let mut stack = vec![root];
    let mut seen = BTreeSet::new();
    while let Some(p) = stack.pop() {
        if !seen.insert(p) {
            continue;
        }
        out.push(p);
        if let Some(k) = kids.get(&p) {
            stack.extend(k.iter().copied());
        }
    }
    out
}

#[cfg(windows)]
mod nt {
    use windows_sys::Win32::Foundation::HANDLE;
    pub const PROCESS_SUSPEND_RESUME: u32 = 0x0800;
    unsafe extern "system" {
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> HANDLE;
        pub fn CloseHandle(h: HANDLE) -> i32;
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        pub fn NtSuspendProcess(h: HANDLE) -> i32;
        pub fn NtResumeProcess(h: HANDLE) -> i32;
    }
}

#[cfg(windows)]
fn act(pid: u32, suspend: bool) -> bool {
    unsafe {
        let h = nt::OpenProcess(nt::PROCESS_SUSPEND_RESUME, 0, pid);
        if h.is_null() {
            return false;
        }
        let st = if suspend {
            nt::NtSuspendProcess(h)
        } else {
            nt::NtResumeProcess(h)
        };
        nt::CloseHandle(h);
        st >= 0
    }
}

#[cfg(not(windows))]
fn act(pid: u32, suspend: bool) -> bool {
    std::process::Command::new("kill")
        .arg(if suspend { "-STOP" } else { "-CONT" })
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// suspend the tree under `root`; returns the pids actually suspended (to resume later)
pub fn suspend_tree(sys: &System, root: u32) -> Vec<u32> {
    tree(sys, root)
        .into_iter()
        .filter(|&p| act(p, true))
        .collect()
}

/// resume pids (children first); returns how many were resumed
pub fn resume_all(pids: &[u32]) -> usize {
    pids.iter().rev().filter(|&&p| act(p, false)).count()
}
