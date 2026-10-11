//! Plain native, read-only statistics viewer on the tray UI thread.
use crate::{
    settings::data_directory,
    storage::{Statistics, read_statistics},
};
use std::{
    cell::Cell,
    path::PathBuf,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject},
    System::LibraryLoader::GetModuleHandleW,
    UI::{Controls::EM_SETLIMITTEXT, WindowsAndMessaging::*},
};
thread_local! { static WINDOW: Cell<HWND> = const { Cell::new(null_mut()) }; }
const REPORT: i32 = 201;
const REFRESH: usize = 202;
const CLOSE: usize = 2;
struct Context {
    path: Option<PathBuf>,
}

pub fn show() {
    show_for_path(data_directory().ok().map(|dir| dir.join("dat.db")));
}
pub(crate) fn show_for_path(path: Option<PathBuf>) {
    WINDOW.with(|slot| unsafe {
        let existing = slot.get();
        if !existing.is_null() {
            ShowWindow(existing, SW_RESTORE);
            SetForegroundWindow(existing);
            refresh(existing);
            return;
        }
        let class = wide("DatStatistics");
        let instance = GetModuleHandleW(null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as _,
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let window = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class.as_ptr(),
            wide("DAT Statistics").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            850,
            600,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            eprintln!(
                "Could not create Statistics window: {}",
                std::io::Error::last_os_error()
            );
            return;
        }
        slot.set(window);
        SetWindowLongPtrW(
            window,
            GWLP_USERDATA,
            Box::into_raw(Box::new(Context { path })) as isize,
        );
        for (class, label, style, id) in [
            (
                "EDIT",
                "",
                WS_VSCROLL
                    | WS_HSCROLL
                    | WS_TABSTOP
                    | ES_MULTILINE as u32
                    | ES_READONLY as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_AUTOHSCROLL as u32,
                REPORT,
            ),
            (
                "BUTTON",
                "Refresh",
                WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
                REFRESH as i32,
            ),
            ("BUTTON", "Close", WS_TABSTOP, CLOSE as i32),
        ] {
            let child = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(label).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                1,
                1,
                window,
                id as usize as HMENU,
                instance,
                null(),
            );
            if child.is_null() {
                eprintln!("Could not create Statistics controls");
                DestroyWindow(window);
                return;
            }
            SendMessageW(
                child,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as usize,
                1,
            );
        }
        SendDlgItemMessageW(window, REPORT, EM_SETLIMITTEXT, 0, 0);
        resize(window);
        refresh(window);
        ShowWindow(window, SW_SHOW);
        SetForegroundWindow(window);
    });
}
pub fn handle_dialog_message(message: &MSG) -> bool {
    WINDOW.with(|slot| {
        let window = slot.get();
        !window.is_null() && unsafe { IsDialogMessageW(window, message) } != 0
    })
}
pub fn close() {
    WINDOW.with(|slot| {
        if !slot.get().is_null() {
            unsafe {
                DestroyWindow(slot.get());
            }
        }
    });
}
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND if wparam & 0xffff == REFRESH || wparam & 0xffff == 1 => {
            refresh(window);
            0
        }
        WM_COMMAND if wparam & 0xffff == CLOSE => {
            unsafe {
                DestroyWindow(window);
            }
            0
        }
        WM_SIZE => {
            resize(window);
            0
        }
        WM_CLOSE => {
            unsafe {
                DestroyWindow(window);
            }
            0
        }
        WM_NCDESTROY => {
            let context = unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, 0) as *mut Context };
            if !context.is_null() {
                drop(unsafe { Box::from_raw(context) });
            }
            WINDOW.with(|slot| slot.set(null_mut()));
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
fn resize(window: HWND) {
    unsafe {
        let mut rect: RECT = std::mem::zeroed();
        GetClientRect(window, &mut rect);
        let width = (rect.right - 24).max(1);
        let height = (rect.bottom - 62).max(1);
        MoveWindow(GetDlgItem(window, REPORT), 12, 12, width, height, 1);
        MoveWindow(
            GetDlgItem(window, REFRESH as i32),
            12,
            height + 24,
            100,
            28,
            1,
        );
        MoveWindow(
            GetDlgItem(window, CLOSE as i32),
            124,
            height + 24,
            100,
            28,
            1,
        );
    }
}
fn refresh(window: HWND) {
    let context = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *const Context };
    if context.is_null() {
        return;
    }
    let path = unsafe { &*context }.path.as_deref();
    let report = match path {
        Some(path) => match read_statistics(path) {
            Ok(stats) => format_statistics(&stats),
            Err(error) => {
                eprintln!("Could not read statistics: {error:#}");
                format!(
                    "Statistics are unavailable: {error:#}\r\n\r\nThe database may be busy or inaccessible. Try Refresh. Tracking continues independently."
                )
            }
        },
        None => "Statistics are unavailable: the local data directory could not be located.".into(),
    };
    unsafe {
        SetDlgItemTextW(window, REPORT, wide(&report).as_ptr());
    }
}
fn format_statistics(stats: &Statistics) -> String {
    let mut text = String::from(
        "Saved local statistics — use Refresh for the latest database checkpoint (normally every 30 seconds).\r\nOnly enabled collection periods are represented; disabling collection retains earlier records.\r\n\r\nACTIVITIES\r\n\r\n",
    );
    if stats.activities.is_empty() {
        text.push_str("No saved activity data yet. Enable cumulative time or session history in Settings and run a configured activity.\r\n");
    }
    for row in &stats.activities {
        text.push_str(&format!("{}\r\n  Total/Open: {}    Active/Foreground: {}{}\r\n  Recorded sessions: {}    Peak window instances: {}    Peak processes: {}\r\n  Last observed session: {}\r\n\r\n",
            row.name, duration(row.open_ms), duration(row.active_ms), if row.session_totals_only { " (recorded sessions only)" } else { " (saved cumulative totals)" },
            row.sessions, row.peak_window_instances.map(|v|v.to_string()).unwrap_or_else(||"unavailable".into()),
            row.peak_processes.map(|v|v.to_string()).unwrap_or_else(||"unavailable".into()),
            row.last_session.as_deref().unwrap_or("No sessions recorded")));
    }
    text.push_str("REMINDERS\r\n\r\n");
    if stats.reminders.is_empty() {
        text.push_str("No detailed reminder data yet. Enable detailed statistics in Settings to collect new reminder occurrences.\r\n");
    }
    for row in &stats.reminders {
        text.push_str(&format!("{}\r\n  Completed: {}    Snoozes (all recorded occurrences): {}\r\n  Average response time (completed only): {}\r\n\r\n",
            row.name, row.completed, row.snoozes,
            row.average_response_ms.map(duration).unwrap_or_else(||"No completions recorded".into())));
    }
    text.push_str("Session counts include current/checkpointed observation segments. Historical process counts are not window counts.\r\nWindow-instance peaks are available only for sessions recorded after the window-count update.\r\n");
    text
}
fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    format!(
        "{}h {:02}m {:02}s",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_state_and_duration() {
        let text = format_statistics(&Statistics::default());
        assert!(text.contains("No saved activity data"));
        assert!(text.contains("No detailed reminder data"));
        assert_eq!(duration(5_523_000), "1h 32m 03s");
    }
}
