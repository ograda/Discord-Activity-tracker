//! Small native settings window, owned by the existing tray UI thread.
use crate::settings::{PresenceDisplay, Settings, SharedSettings, TrackingMode, data_directory};
use std::{
    cell::Cell,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::{COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject},
    System::{
        LibraryLoader::GetModuleHandleW,
        SystemServices::{SS_CENTER, SS_NOPREFIX, SS_NOTIFY},
    },
    UI::{Controls::*, WindowsAndMessaging::*},
};
thread_local! {static WINDOW: Cell<HWND> = const {Cell::new(null_mut())};}
const MODE: i32 = 101;
const PERSIST: i32 = 102;
const HISTORY: i32 = 103;
const INSTANCES: i32 = 104;
const STATISTICS: i32 = 105;
const PRESENCE: i32 = 106;
const FOCUSED: i32 = 108;
const OPEN_STATISTICS: usize = 107;
const SAVE: usize = 1;
const CANCEL: usize = 2;

struct WindowState {
    settings: SharedSettings,
    tooltip: HWND,
    // Win32 keeps these pointers; each allocation lives until the window closes.
    help_texts: Vec<Box<[u16]>>,
}
const HELP: [(i32, i32, &str); 8] = [
    (
        FOCUSED,
        380,
        "Shows the currently focused tracked activity instead of rotating through open activities. When no tracked activity is focused, Discord displays Activity Tracker.",
    ),
    (
        MODE,
        42,
        "Total/Open counts time while a tracked activity is running. Active/Foreground counts only time while one of its windows owns the foreground.",
    ),
    (
        PERSIST,
        82,
        "Keeps accumulated activity time between activity and DAT restarts. Disabling this does not delete previously saved totals.",
    ),
    (
        INSTANCES,
        114,
        "Displays multiple instances when more than one instance is detected. The counting method can be configured per activity in activities.xml.",
    ),
    (
        HISTORY,
        190,
        "Stores observed activity sessions, including when an activity starts, stops, and its open/active durations.",
    ),
    (
        STATISTICS,
        222,
        "Stores additional local events such as foreground changes, process and window changes, reminder completions, response times, and snoozes.",
    ),
    (
        OPEN_STATISTICS as i32,
        254,
        "Opens the locally stored activity and reminder statistics.",
    ),
    (
        PRESENCE,
        344,
        "Adds the number of other currently open tracked activities to the Discord activity details.",
    ),
];

pub fn show(settings: SharedSettings) {
    WINDOW.with(|slot| unsafe {
        let existing = slot.get();
        if !existing.is_null() {
            ShowWindow(existing, SW_RESTORE);
            SetForegroundWindow(existing);
            return;
        }
        let instance = GetModuleHandleW(null());
        let class = wide("DatSettings");
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
            wide("DAT Settings").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            540,
            560,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            eprintln!(
                "Could not create Settings window: {}",
                std::io::Error::last_os_error()
            );
            return;
        }
        slot.set(window);
        let current = settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
        SetWindowLongPtrW(
            window,
            GWLP_USERDATA,
            Box::into_raw(Box::new(WindowState {
                settings,
                tooltip: null_mut(),
                help_texts: Vec::new(),
            })) as isize,
        );
        if !build_controls(window, &current) {
            eprintln!("Could not create Settings controls");
            DestroyWindow(window);
            return;
        }
        ShowWindow(window, SW_SHOW);
        SetForegroundWindow(window);
    });
}
// Route keyboard navigation through the existing message loop.
pub fn handle_dialog_message(message: &MSG) -> bool {
    WINDOW.with(|slot| {
        let window = slot.get();
        !window.is_null() && unsafe { IsDialogMessageW(window, message) } != 0
    })
}
pub fn close() {
    WINDOW.with(|slot| {
        let window = slot.get();
        if !window.is_null() {
            unsafe {
                DestroyWindow(window);
            }
        }
    });
}
fn build_controls(window: HWND, s: &Settings) -> bool {
    let items = [
        ("STATIC", "TRACKING", 0, 18, 16, 450, 22, 301),
        ("STATIC", "Tracking mode", 0, 18, 45, 175, 24, 0),
        (
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL | WS_TABSTOP,
            200,
            42,
            275,
            120,
            MODE,
        ),
        (
            "BUTTON",
            "Persist cumulative activity time",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            18,
            82,
            457,
            25,
            PERSIST,
        ),
        (
            "BUTTON",
            "Detect/show multiple instances",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            18,
            114,
            457,
            25,
            INSTANCES,
        ),
        (
            "STATIC",
            "STATISTICS & HISTORY (local only)",
            SS_NOPREFIX,
            18,
            160,
            450,
            22,
            302,
        ),
        (
            "BUTTON",
            "Store activity/session history",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            18,
            190,
            457,
            25,
            HISTORY,
        ),
        (
            "BUTTON",
            "Collect detailed interaction and process statistics",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            18,
            222,
            457,
            25,
            STATISTICS,
        ),
        (
            "BUTTON",
            "Open Statistics",
            WS_TABSTOP,
            18,
            254,
            155,
            28,
            OPEN_STATISTICS as i32,
        ),
        ("STATIC", "DISCORD PRESENCE", 0, 18, 316, 450, 22, 303),
        (
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL | WS_TABSTOP,
            18,
            344,
            457,
            120,
            PRESENCE,
        ),
        (
            "BUTTON",
            "Display focused activity instead of carousel",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            18,
            380,
            457,
            25,
            FOCUSED,
        ),
        (
            "STATIC",
            "Changes apply immediately after Save. Existing history is kept.",
            0,
            18,
            434,
            490,
            22,
            0,
        ),
        (
            "BUTTON",
            "Save",
            BS_DEFPUSHBUTTON as u32 | WS_TABSTOP,
            280,
            472,
            95,
            28,
            SAVE as i32,
        ),
        (
            "BUTTON",
            "Cancel",
            WS_TABSTOP,
            390,
            472,
            95,
            28,
            CANCEL as i32,
        ),
    ];
    for (class, text, style, x, y, width, height, id) in items {
        unsafe {
            let control = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                x,
                y,
                width,
                height,
                window,
                id as usize as HMENU,
                GetModuleHandleW(null()),
                null(),
            );
            if control.is_null() {
                return false;
            }
            SendMessageW(
                control,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as usize,
                1,
            );
        }
    }
    unsafe {
        for text in ["Total / Open", "Active / Foreground"] {
            SendDlgItemMessageW(window, MODE, CB_ADDSTRING, 0, wide(text).as_ptr() as isize);
        }
        SendDlgItemMessageW(
            window,
            MODE,
            CB_SETCURSEL,
            usize::from(s.tracking_mode == TrackingMode::Active),
            0,
        );
        for text in ["Include other activity count", "Selected activity only"] {
            SendDlgItemMessageW(
                window,
                PRESENCE,
                CB_ADDSTRING,
                0,
                wide(text).as_ptr() as isize,
            );
        }
        SendDlgItemMessageW(
            window,
            PRESENCE,
            CB_SETCURSEL,
            usize::from(s.presence_display == PresenceDisplay::SelectedOnly),
            0,
        );
        for (id, checked) in [
            (PERSIST, s.persist_activity_time),
            (FOCUSED, s.display_focused_activity),
            (HISTORY, s.session_history),
            (INSTANCES, s.multiple_instances),
            (STATISTICS, s.detailed_statistics),
        ] {
            SendDlgItemMessageW(window, id, BM_SETCHECK, usize::from(checked), 0);
        }
    }
    build_help(window)
}
fn build_help(window: HWND) -> bool {
    unsafe {
        let init = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_WIN95_CLASSES,
        };
        if InitCommonControlsEx(&init) == 0 {
            eprintln!(
                "Could not initialize tooltip controls: {}",
                std::io::Error::last_os_error()
            );
            return false;
        }
        let tooltip = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            TOOLTIPS_CLASSW,
            null(),
            WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            window,
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        if tooltip.is_null() {
            eprintln!(
                "Could not create tooltip: {}",
                std::io::Error::last_os_error()
            );
            return false;
        }
        let state = &mut *(GetWindowLongPtrW(window, GWLP_USERDATA) as *mut WindowState);
        state.tooltip = tooltip;
        SendMessageW(tooltip, TTM_SETMAXTIPWIDTH, 0, 400);
        for (id, y, text) in HELP {
            let indicator = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide("?").as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_NOTIFY | SS_CENTER,
                if id == OPEN_STATISTICS as i32 {
                    183
                } else {
                    490
                },
                y,
                20,
                24,
                window,
                (id + 1000) as usize as HMENU,
                GetModuleHandleW(null()),
                null(),
            );
            if indicator.is_null() {
                eprintln!(
                    "Could not create help indicator {id}: {}",
                    std::io::Error::last_os_error()
                );
                return false;
            }
            SendMessageW(
                indicator,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as usize,
                1,
            );
            let mut text = wide(text).into_boxed_slice();
            // V2 structure size also works with unthemed common-controls v5.
            // The trailing lpReserved member requires a v6 activation context.
            let info = TTTOOLINFOW {
                cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as u32,
                uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                hwnd: window,
                uId: indicator as usize,
                lpszText: text.as_mut_ptr(),
                ..std::mem::zeroed()
            };
            state.help_texts.push(text);
            if SendMessageW(tooltip, TTM_ADDTOOLW, 0, &info as *const _ as isize) == 0 {
                eprintln!("Could not register help text {id}");
                return false;
            }
        }
        true
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND if wparam & 0xffff == OPEN_STATISTICS => {
            crate::statistics_window::show();
            0
        }
        WM_COMMAND if wparam & 0xffff == SAVE => {
            let shared = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *const WindowState };
            if shared.is_null() {
                return 0;
            }
            let checked = |id| unsafe { SendDlgItemMessageW(window, id, BM_GETCHECK, 0, 0) == 1 };
            let s = Settings {
                tracking_mode: if unsafe { SendDlgItemMessageW(window, MODE, CB_GETCURSEL, 0, 0) }
                    == 1
                {
                    TrackingMode::Active
                } else {
                    TrackingMode::Total
                },
                persist_activity_time: checked(PERSIST),
                display_focused_activity: checked(FOCUSED),
                session_history: checked(HISTORY),
                multiple_instances: checked(INSTANCES),
                detailed_statistics: checked(STATISTICS),
                presence_display: if unsafe {
                    SendDlgItemMessageW(window, PRESENCE, CB_GETCURSEL, 0, 0)
                } == 1
                {
                    PresenceDisplay::SelectedOnly
                } else {
                    PresenceDisplay::ActivityNames
                },
            };
            match data_directory().and_then(|dir| s.save(&dir.join("settings.xml"))) {
                Ok(()) => {
                    *unsafe { &*shared }
                        .settings
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = s;
                    unsafe {
                        DestroyWindow(window);
                    }
                }
                Err(error) => {
                    eprintln!("Settings save failed: {error:#}");
                    unsafe {
                        MessageBoxW(
                            window,
                            wide(&format!(
                                "Could not save settings: {error:#}\nChanges have not been applied."
                            ))
                            .as_ptr(),
                            wide("DAT Settings").as_ptr(),
                            MB_OK | MB_ICONERROR,
                        );
                    }
                }
            }
            0
        }
        WM_COMMAND if wparam & 0xffff == CANCEL => {
            unsafe {
                DestroyWindow(window);
            }
            0
        }
        WM_CLOSE => {
            unsafe {
                DestroyWindow(window);
            }
            0
        }
        WM_DESTROY => {
            let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *mut WindowState };
            if !state.is_null() {
                unsafe {
                    // Remove subclasses/tool references before releasing their text.
                    let tooltip = (*state).tooltip;
                    (*state).tooltip = null_mut();
                    if IsWindow(tooltip) != 0 {
                        DestroyWindow(tooltip);
                    }
                }
            }
            0
        }
        WM_NCDESTROY => {
            let shared = unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, 0) as *mut WindowState };
            if !shared.is_null() {
                drop(unsafe { Box::from_raw(shared) });
            }
            WINDOW.with(|slot| slot.set(null_mut()));
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
pub(crate) fn assert_native_help_layout(window: HWND) -> HWND {
    unsafe {
        let state = &*(GetWindowLongPtrW(window, GWLP_USERDATA) as *const WindowState);
        assert_ne!(IsWindow(state.tooltip), 0);
        assert_eq!(GetWindow(state.tooltip, GW_OWNER), window);
        assert_eq!(
            SendMessageW(state.tooltip, TTM_GETTOOLCOUNT, 0, 0),
            HELP.len() as isize
        );
        for (id, _, expected) in HELP {
            let indicator = GetDlgItem(window, id + 1000);
            assert_ne!(IsWindow(indicator), 0);
            assert_eq!(
                GetWindowLongPtrW(indicator, GWL_STYLE) as u32 & WS_TABSTOP,
                0
            );
            let mut registered_text = [0u16; 1024];
            let mut info = TTTOOLINFOW {
                cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as u32,
                hwnd: window,
                uId: indicator as usize,
                lpszText: registered_text.as_mut_ptr(),
                ..std::mem::zeroed()
            };
            assert_ne!(
                SendMessageW(
                    state.tooltip,
                    TTM_GETTOOLINFOW,
                    0,
                    &mut info as *mut _ as isize
                ),
                0
            );
            assert_eq!(
                info.uFlags & (TTF_SUBCLASS | TTF_IDISHWND),
                TTF_SUBCLASS | TTF_IDISHWND
            );
            let expected = wide(expected);
            assert_eq!(&registered_text[..expected.len()], expected);
            let mut control_rect = std::mem::zeroed();
            let mut help_rect = std::mem::zeroed();
            GetWindowRect(GetDlgItem(window, id), &mut control_rect);
            GetWindowRect(indicator, &mut help_rect);
            assert!(control_rect.right <= help_rect.left);
        }
        let mut heading = [0u16; 80];
        GetWindowTextW(
            GetDlgItem(window, 302),
            heading.as_mut_ptr(),
            heading.len() as i32,
        );
        assert!(String::from_utf16_lossy(&heading).starts_with("STATISTICS & HISTORY"));
        let rect = |id| {
            let mut r = std::mem::zeroed();
            GetWindowRect(GetDlgItem(window, id), &mut r);
            r
        };
        assert!(rect(INSTANCES).bottom < rect(302).top);
        assert!(rect(302).bottom <= rect(HISTORY).top);
        assert!(rect(HISTORY).bottom <= rect(STATISTICS).top);
        assert!(rect(OPEN_STATISTICS as i32).bottom < rect(303).top);
        state.tooltip
    }
}
