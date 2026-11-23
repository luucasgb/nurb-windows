//! The few things macOS and Windows disagree about outside the sandbox: how a
//! child and everything it starts are killed together, the home folder, and
//! where the user's PATH lives. macOS keeps its process groups and signals
//! exactly as before; Windows has neither, so a "group" there is the process
//! tree under the child's pid, and every signal ends that tree.

use std::path::PathBuf;

#[cfg(unix)]
pub(crate) use libc::{SIGINT, SIGKILL, SIGTERM};

// Windows has no signals. The values only say what the caller meant; all three
// end the tree (a Ctrl-C cannot reach a child with no console of its own, and
// the Claude driver's SIGINT already means "stop and exit").
#[cfg(windows)]
pub(crate) const SIGINT: i32 = 2;
#[cfg(windows)]
pub(crate) const SIGKILL: i32 = 9;
#[cfg(windows)]
pub(crate) const SIGTERM: i32 = 15;

/// Puts the child in a group of its own, so `killpg` takes the whole tree
/// rather than orphaning what it started. On Windows it also keeps a console
/// child of the windowless app from flashing a console window.
pub(crate) trait OwnGroup {
    fn own_group(&mut self) -> &mut Self;
}

impl OwnGroup for std::process::Command {
    fn own_group(&mut self) -> &mut Self {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            self.process_group(0)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(windows::CHILD_FLAGS)
        }
    }
}

impl OwnGroup for tokio::process::Command {
    fn own_group(&mut self) -> &mut Self {
        #[cfg(unix)]
        {
            self.process_group(0)
        }
        #[cfg(windows)]
        {
            self.creation_flags(windows::CHILD_FLAGS)
        }
    }
}

/// For a short check whose output is read and that is never killed as a group
/// (`claude auth status`, `python -c`): on Windows, no console window. A
/// console child of the windowless app otherwise flashes one each time, and
/// the agent status checks run every time the rail refreshes. Nothing on macOS.
pub(crate) trait NoWindow {
    fn no_window(&mut self) -> &mut Self;
}

impl NoWindow for std::process::Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }
        self
    }
}

impl NoWindow for tokio::process::Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        self.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        self
    }
}

/// Signals the group a child made with `own_group`. A pgid of 0 or less is
/// refused, because on macOS it would be the app's own group.
pub(crate) fn killpg(pgid: i32, signal: i32) {
    if pgid <= 0 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::killpg(pgid, signal);
    }
    #[cfg(windows)]
    {
        let _ = signal;
        windows::kill_tree(pgid as u32);
    }
}

/// Signals one process.
pub(crate) fn kill(pid: i32, signal: i32) {
    if pid <= 0 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid, signal);
    }
    #[cfg(windows)]
    {
        let _ = signal;
        windows::terminate(pid as u32);
    }
}

/// Whether the process is still running.
pub(crate) fn alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid, 0) == 0
    }
    #[cfg(windows)]
    {
        windows::alive(pid as u32)
    }
}

/// Whether nobody owns the process any more. Windows never reparents, so a
/// dead parent's pid lingers on the child (macOS asks whether it is launchd's).
#[cfg(windows)]
pub(crate) fn orphaned(pid: u32) -> bool {
    windows::orphaned(pid)
}

/// The home folder: `$HOME` on macOS, the profile folder on Windows, where
/// Python's `Path.home()` and every agent CLI put their dotfiles. A Windows
/// app started from Explorer has no `HOME` at all.
pub(crate) fn home() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// An executable's file name: `uv` on macOS, `uv.exe` on Windows.
pub(crate) fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

#[cfg(windows)]
pub(crate) mod windows {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{
        CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, TerminateProcess,
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_TERMINATE,
    };

    /// No console window for a console child of a GUI app, and its own Ctrl-C
    /// group so a dev terminal's Ctrl-C reaches the app, not the children.
    pub(crate) const CHILD_FLAGS: u32 = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;

    pub(crate) fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn open(pid: u32, access: u32) -> Option<Handle> {
        let handle = unsafe { OpenProcess(access, 0, pid) };
        (!handle.is_null()).then_some(Handle(handle))
    }

    fn started(pid: u32) -> Option<u64> {
        let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        let ok = unsafe {
            GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user)
        };
        (ok != 0).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }

    /// Every running process as (pid, parent pid).
    fn processes() -> Vec<(u32, u32)> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let snapshot = Handle(snapshot);
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut all = Vec::new();
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
        while more {
            all.push((entry.th32ProcessID, entry.th32ParentProcessID));
            more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
        }
        all
    }

    /// The pids under `root`, root first. A recorded parent pid can belong to a
    /// process that died and whose pid was reused, so a child only counts if it
    /// started after the parent it names: a stranger is never killed for
    /// inheriting a recycled number.
    pub(crate) fn tree(root: u32) -> Vec<u32> {
        let all = processes();
        let mut found = vec![root];
        let mut next = 0;
        while next < found.len() {
            let parent = found[next];
            next += 1;
            let Some(parent_started) = started(parent) else {
                continue;
            };
            for &(pid, ppid) in &all {
                if ppid == parent
                    && pid != parent
                    && !found.contains(&pid)
                    && started(pid).is_some_and(|child| child >= parent_started)
                {
                    found.push(pid);
                }
            }
        }
        found
    }

    pub(crate) fn terminate(pid: u32) {
        if let Some(process) = open(pid, PROCESS_TERMINATE) {
            unsafe {
                TerminateProcess(process.0, 1);
            }
        }
    }

    /// The root goes first so it cannot start anything new, then everything
    /// that was under it when the tree was read.
    pub(crate) fn kill_tree(root: u32) {
        for pid in tree(root) {
            terminate(pid);
        }
    }

    pub(crate) fn alive(pid: u32) -> bool {
        let Some(process) = open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
            return false;
        };
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(process.0, &mut code) != 0 && code == STILL_ACTIVE as u32 }
    }

    pub(crate) fn orphaned(pid: u32) -> bool {
        let Some(&(_, parent)) = processes().iter().find(|(p, _)| *p == pid) else {
            return false;
        };
        match (started(parent), started(pid)) {
            // The parent pid now names a younger process: the real parent died.
            (Some(parent_started), Some(child_started)) => parent_started > child_started,
            _ => true,
        }
    }

    /// A string value from the registry, environment references expanded.
    pub(crate) fn registry_string(
        root: windows_sys::Win32::System::Registry::HKEY,
        key: &str,
        value: &str,
    ) -> Option<String> {
        use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ};
        let key = wide(OsStr::new(key));
        let value = wide(OsStr::new(value));
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
        let mut size = 0u32;
        let status = unsafe {
            RegGetValueW(root, key.as_ptr(), value.as_ptr(), flags, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
        };
        if status != 0 || size == 0 {
            return None;
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                flags,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != 0 {
            return None;
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..end]))
    }

    /// "Windows 11 24H2 (26100)", for the about box and setup reports.
    pub(crate) fn os_version() -> Option<String> {
        use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
        const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let build = registry_string(HKEY_LOCAL_MACHINE, KEY, "CurrentBuild")?;
        // The product name still says "Windows 10" on Windows 11; the build
        // number is the honest signal.
        let name = if build.parse::<u32>().unwrap_or(0) >= 22000 {
            "Windows 11"
        } else {
            "Windows 10"
        };
        let release = registry_string(HKEY_LOCAL_MACHINE, KEY, "DisplayVersion");
        Some(match release {
            Some(release) => format!("{name} {release} ({build})"),
            None => format!("{name} ({build})"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn killpg_takes_the_whole_tree() {
        // A shell that starts a grandchild and waits on it: killing the group
        // must end both, not just the shell.
        #[cfg(unix)]
        let mut command = {
            let mut c = std::process::Command::new("/bin/sh");
            c.args(["-c", "sleep 30 & wait"]);
            c
        };
        #[cfg(windows)]
        let mut command = {
            let mut c = std::process::Command::new("cmd.exe");
            c.args(["/d", "/c", "ping -n 30 127.0.0.1 >NUL"]);
            c
        };
        let mut child = command.own_group().spawn().unwrap();
        let pid = child.id() as i32;
        std::thread::sleep(std::time::Duration::from_millis(500));
        #[cfg(windows)]
        let tree = windows::tree(pid as u32);
        #[cfg(windows)]
        assert!(tree.len() >= 2, "the grandchild is part of the tree: {tree:?}");
        assert!(alive(pid));
        killpg(pid, SIGKILL);
        let _ = child.wait();
        #[cfg(windows)]
        for pid in tree {
            assert!(!windows::alive(pid), "{pid} survived the tree kill");
        }
        assert!(!alive(pid));
    }

    #[test]
    fn the_home_folder_is_found() {
        assert!(home().is_some_and(|home| home.is_dir()));
    }
}
