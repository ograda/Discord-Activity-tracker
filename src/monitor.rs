use std::collections::HashMap;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, RECT},
    Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
    UI::WindowsAndMessaging::*,
};

#[derive(Debug, Default, Clone)]
pub struct ProcessSnapshot {
    pub processes: HashMap<u32, String>,
    // One PID per eligible window, deliberately repeated for multiple windows
    // owned by the same process. Handles and titles stay inside this module.
    pub application_windows: Vec<u32>,
    pub foreground_pid: Option<u32>,
}
pub struct ProcessMonitor {
    system: System,
}
impl ProcessMonitor {
    pub fn new() -> Self {
        Self {
            system: System::new(),
        }
    }
    pub fn observe(&mut self, refresh_processes: bool) -> ProcessSnapshot {
        if refresh_processes {
            self.system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
        }
        let processes = self
            .system
            .processes()
            .iter()
            .map(|(pid, process)| {
                (
                    pid.as_u32(),
                    normalize_process_name(&process.name().to_string_lossy()),
                )
            })
            .collect();
        let mut application_windows = Vec::<u32>::new();
        unsafe {
            if EnumWindows(
                Some(collect_window),
                &mut application_windows as *mut Vec<u32> as LPARAM,
            ) == 0
            {
                eprintln!(
                    "Could not enumerate application windows: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        let foreground_pid = unsafe { window_pid(GetForegroundWindow()) };
        ProcessSnapshot {
            processes,
            application_windows,
            foreground_pid,
        }
    }
}
pub fn normalize_process_name(name: &str) -> String {
    name.trim().to_lowercase()
}

unsafe fn window_pid(window: HWND) -> Option<u32> {
    let mut pid = 0;
    if !window.is_null() {
        unsafe {
            GetWindowThreadProcessId(window, &mut pid);
        }
    }
    (pid != 0).then_some(pid)
}
unsafe extern "system" fn collect_window(window: HWND, data: LPARAM) -> i32 {
    // EnumWindows supplies top-level windows. Exclude helper/tool/no-activate
    // windows, invisible/cloaked windows, and ordinary owned dialogs. Explicit
    // APPWINDOW windows can be independent taskbar applications despite ownership.
    unsafe {
        let style = GetWindowLongPtrW(window, GWL_STYLE) as u32;
        let extended = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
        if IsWindowVisible(window) == 0
            || style & WS_CHILD != 0
            || extended & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) != 0
            || (!GetWindow(window, GW_OWNER).is_null() && extended & WS_EX_APPWINDOW == 0)
            || GetWindowTextLengthW(window) == 0
        {
            return 1;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED as u32,
            &mut cloaked as *mut u32 as _,
            std::mem::size_of::<u32>() as u32,
        ) >= 0
            && cloaked != 0
        {
            return 1;
        }
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(window, &mut rect) == 0
            || rect.right <= rect.left
            || rect.bottom <= rect.top
        {
            return 1;
        }
        if let Some(pid) = window_pid(window) {
            (&mut *(data as *mut Vec<u32>)).push(pid);
        }
    }
    1
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn native_window_count_filters_helpers_and_counts_two_windows_in_one_pid() {
        use std::ptr::{null, null_mut};
        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(Some(0)).collect()
        }
        unsafe {
            let previous = GetForegroundWindow();
            let instance = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(null());
            let create = |extended, style, owner, title: &str| {
                CreateWindowExW(
                    extended,
                    wide("STATIC").as_ptr(),
                    wide(title).as_ptr(),
                    style,
                    50,
                    50,
                    250,
                    100,
                    owner,
                    null_mut(),
                    instance,
                    null(),
                )
            };
            let own_pid = std::process::id();
            let mut monitor = ProcessMonitor::new();
            let count = |snapshot: ProcessSnapshot| {
                snapshot
                    .application_windows
                    .iter()
                    .filter(|pid| **pid == own_pid)
                    .count()
            };
            let baseline = count(monitor.observe(true));
            let first = create(
                WS_EX_APPWINDOW,
                WS_OVERLAPPEDWINDOW,
                null_mut(),
                "DAT instance smoke A",
            );
            assert!(!first.is_null());
            ShowWindow(first, SW_SHOWNOACTIVATE);
            assert_eq!(count(monitor.observe(false)), baseline + 1);
            let second = create(
                WS_EX_APPWINDOW,
                WS_OVERLAPPEDWINDOW,
                null_mut(),
                "DAT instance smoke B",
            );
            assert!(!second.is_null());
            ShowWindow(second, SW_SHOWNOACTIVATE);
            let tool = create(
                WS_EX_TOOLWINDOW,
                WS_POPUP | WS_VISIBLE,
                null_mut(),
                "DAT tool",
            );
            let owned = create(0, WS_POPUP | WS_VISIBLE, first, "DAT owned dialog");
            let hidden = create(0, WS_OVERLAPPEDWINDOW, null_mut(), "DAT hidden helper");
            let no_activate = create(
                WS_EX_NOACTIVATE,
                WS_POPUP | WS_VISIBLE,
                null_mut(),
                "DAT nonactivating helper",
            );
            assert_eq!(count(monitor.observe(false)), baseline + 2);
            ShowWindow(first, SW_MINIMIZE);
            assert_eq!(count(monitor.observe(false)), baseline + 2);
            DestroyWindow(second);
            assert_eq!(count(monitor.observe(false)), baseline + 1);
            for window in [tool, owned, hidden, no_activate, first] {
                if !window.is_null() {
                    DestroyWindow(window);
                }
            }
            if !previous.is_null() {
                SetForegroundWindow(previous);
            }
        }
    }

    use super::*;
    #[test]
    fn real_windows_snapshot_contains_current_process() {
        let snapshot = ProcessMonitor::new().observe(true);
        let executable = std::env::current_exe().unwrap();
        let expected = normalize_process_name(&executable.file_name().unwrap().to_string_lossy());
        assert_eq!(snapshot.processes.get(&std::process::id()), Some(&expected));
        assert_eq!(normalize_process_name("  WoW.EXE  "), "wow.exe");
    }
}
