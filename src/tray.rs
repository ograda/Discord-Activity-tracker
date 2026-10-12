use std::{
    mem::size_of,
    ptr::{null, null_mut},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc::Sender,
    },
    thread,
};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
            Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
            DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW,
            GetSystemMetrics, GetWindowLongPtrW, HMENU, KillTimer, MF_BYCOMMAND, MF_SEPARATOR,
            MF_STRING, MSG, ModifyMenuW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
            SM_CXSMICON, SetForegroundWindow, SetTimer, SetWindowLongPtrW, TPM_BOTTOMALIGN,
            TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE,
            WM_COMMAND, WM_DESTROY, WM_ENDSESSION, WM_NCDESTROY, WM_QUERYENDSESSION, WM_RBUTTONUP,
            WM_TIMER, WNDCLASSW,
        },
    },
};

const TRAY_ID: u32 = 1;
const TRAY_CALLBACK: u32 = WM_APP + 1;
const VISIBILITY_RETRY: usize = 10;
#[cfg(test)]
const TEST_VISIBILITY: u32 = WM_APP + 7;
const MENU_VISIBILITY: usize = 1001;
const MENU_EXIT: usize = 1003;
const MENU_SETTINGS: usize = 1004;
const MENU_STATISTICS: usize = 1005;
static SETTINGS: OnceLock<crate::settings::SharedSettings> = OnceLock::new();
static TRAY_WINDOW: AtomicIsize = AtomicIsize::new(0);

static COMMAND_SENDER: OnceLock<Sender<TrayCommand>> = OnceLock::new();

static MENU_HANDLE: AtomicIsize = AtomicIsize::new(0);

struct TrayState {
    normal: crate::tray_icon::TrayIcon,
    muted: Option<crate::tray_icon::TrayIcon>,
    hidden: Arc<AtomicBool>,
    registered: bool,
    applied_hidden: Option<bool>,
    taskbar_created: u32,
}
impl TrayState {
    fn icon(&self, hidden: bool) -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
        if hidden {
            self.muted.as_ref().unwrap_or(&self.normal).0
        } else {
            self.normal.0
        }
    }
}
fn visibility_label(hidden: bool) -> &'static str {
    if hidden {
        "Show activities"
    } else {
        "Hide activities"
    }
}
fn sync_visibility(window: HWND) {
    unsafe {
        let state = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut TrayState;
        if state.is_null() {
            return;
        }
        let state = &mut *state;
        let hidden = state.hidden.load(Ordering::SeqCst);
        let menu = MENU_HANDLE.load(Ordering::SeqCst) as HMENU;
        let label_ok = ModifyMenuW(
            menu,
            MENU_VISIBILITY as u32,
            MF_BYCOMMAND | MF_STRING,
            MENU_VISIBILITY,
            wide(visibility_label(hidden)).as_ptr(),
        ) != 0;
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = window;
        data.uID = TRAY_ID;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = TRAY_CALLBACK;
        data.hIcon = state.icon(hidden);
        copy_tooltip(
            &mut data.szTip,
            if hidden {
                "DAT - Activity details hidden; local tracking continues"
            } else {
                "Discord Activity Tracker"
            },
        );
        let operation = if state.registered {
            NIM_MODIFY
        } else {
            NIM_ADD
        };
        let mut icon_ok = Shell_NotifyIconW(operation, &data) != 0;
        if !icon_ok {
            // Explorer can lose the registration while restarting. Also accept
            // an existing registration when retrying an uncertain failed add.
            icon_ok = Shell_NotifyIconW(
                if state.registered {
                    NIM_ADD
                } else {
                    NIM_MODIFY
                },
                &data,
            ) != 0;
        }
        state.registered = icon_ok;
        if label_ok && icon_ok {
            state.applied_hidden = Some(hidden);
            KillTimer(window, VISIBILITY_RETRY);
        } else {
            eprintln!(
                "Could not synchronize tray visibility; retrying (menu={label_ok}, icon={icon_ok})"
            );
            state.applied_hidden = None;
            SetTimer(window, VISIBILITY_RETRY, 1000, None);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum TrayCommand {
    Reload,
    Exit,
}

pub fn start(
    sender: Sender<TrayCommand>,
    settings: crate::settings::SharedSettings,
    hidden: Arc<AtomicBool>,
) {
    let _ = SETTINGS.set(settings);
    if COMMAND_SENDER.set(sender).is_err() {
        eprintln!("Tray command sender was already initialized.");
        return;
    }

    thread::spawn(move || {
        if let Err(error) = run_tray(hidden) {
            eprintln!("Tray error: {error}");
        }
    });
}

fn run_tray(hidden: Arc<AtomicBool>) -> Result<(), &'static str> {
    unsafe {
        let instance = GetModuleHandleW(null());

        if instance.is_null() {
            return Err("GetModuleHandleW failed");
        }

        let class_name = wide("DiscordActivityTrackerTray");

        let mut window_class: WNDCLASSW = std::mem::zeroed();

        window_class.lpfnWndProc = Some(window_proc);
        window_class.hInstance = instance;
        window_class.lpszClassName = class_name.as_ptr();

        if RegisterClassW(&window_class) == 0 {
            return Err("RegisterClassW failed");
        }

        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );

        if window.is_null() {
            return Err("CreateWindowExW failed");
        }

        let menu = CreatePopupMenu();

        if menu.is_null() {
            DestroyWindow(window);
            return Err("CreatePopupMenu failed");
        }

        append_menu_item(
            menu,
            MENU_VISIBILITY,
            visibility_label(hidden.load(Ordering::SeqCst)),
        )?;
        append_menu_item(menu, MENU_SETTINGS, "Settings...")?;
        append_menu_item(menu, MENU_STATISTICS, "Statistics...")?;

        AppendMenuW(menu, MF_SEPARATOR, 0, null());

        append_menu_item(menu, MENU_EXIT, "Exit")?;

        MENU_HANDLE.store(menu as isize, Ordering::SeqCst);

        // Render from the embedded DAT icon once; retain both HICONs until the
        // shell icon has been deleted. No external icon file or image dependency.
        let size = GetSystemMetrics(SM_CXSMICON).clamp(32, 256);
        let normal = match crate::tray_icon::TrayIcon::load(size) {
            Ok(icon) => icon,
            Err(error) => {
                eprintln!("Tray icon error: {error:#}");
                DestroyWindow(window);
                return Err("could not load tray icon");
            }
        };
        let muted = match normal.muted(size) {
            Ok(icon) => Some(icon),
            Err(error) => {
                eprintln!("Could not composite muted tray icon; retaining normal icon: {error:#}");
                None
            }
        };
        let state = Box::new(TrayState {
            normal,
            muted,
            hidden,
            registered: false,
            applied_hidden: None,
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
        });
        SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(state) as isize);
        sync_visibility(window);
        TRAY_WINDOW.store(window as isize, Ordering::SeqCst);

        let mut message: MSG = std::mem::zeroed();

        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            if crate::settings_window::handle_dialog_message(&message)
                || crate::statistics_window::handle_dialog_message(&message)
            {
                continue;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        Ok(())
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *mut TrayState };
    if !state.is_null() && message != 0 && message == unsafe { &*state }.taskbar_created {
        unsafe {
            (*state).registered = false;
        }
        sync_visibility(window);
        return 0;
    }
    match message {
        windows_sys::Win32::UI::WindowsAndMessaging::WM_SETTINGCHANGE => {
            crate::ui_palette::system_changed();
            0
        }
        #[cfg(test)]
        TEST_VISIBILITY => {
            if state.is_null() {
                return -1;
            }
            let state = unsafe { &*state };
            let hidden = state.hidden.load(Ordering::SeqCst);
            if state.applied_hidden != Some(hidden) || !state.registered {
                return -1;
            }
            if hidden && state.muted.is_none() {
                return -2;
            }
            isize::from(hidden)
        }
        WM_TIMER if wparam == VISIBILITY_RETRY => {
            sync_visibility(window);
            0
        }
        WM_NCDESTROY => {
            let state = unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, 0) as *mut TrayState };
            if !state.is_null() {
                drop(unsafe { Box::from_raw(state) });
            }
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        TRAY_CALLBACK => {
            if lparam as u32 == WM_RBUTTONUP {
                unsafe {
                    show_menu(window);
                }
            }

            0
        }

        WM_COMMAND => {
            let command_id = wparam & 0xffff;

            match command_id {
                MENU_VISIBILITY => {
                    let state =
                        unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *mut TrayState };
                    if !state.is_null() {
                        // The tray thread applies the single canonical flag and
                        // both UI elements in the same command, with no second
                        // queued toggle that could reverse rapid user clicks.
                        unsafe { &*state }.hidden.fetch_xor(true, Ordering::SeqCst);
                        sync_visibility(window);
                    }
                }

                MENU_STATISTICS => {
                    crate::statistics_window::show();
                }
                MENU_SETTINGS => {
                    if let Some(settings) = SETTINGS.get() {
                        crate::settings_window::show(settings.clone());
                    }
                }

                MENU_EXIT => {
                    send_command(TrayCommand::Exit);

                    unsafe {
                        DestroyWindow(window);
                    }
                }

                _ => {}
            }

            0
        }

        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                send_command(TrayCommand::Exit);
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
            crate::settings_window::close();
            crate::statistics_window::close();
            TRAY_WINDOW.store(0, Ordering::SeqCst);
            unsafe {
                remove_tray_icon(window);
                PostQuitMessage(0);
            }

            0
        }

        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

unsafe fn show_menu(window: HWND) {
    let menu: HMENU = MENU_HANDLE.load(Ordering::SeqCst) as HMENU;

    if menu.is_null() {
        return;
    }

    let mut cursor = POINT { x: 0, y: 0 };

    unsafe {
        if GetCursorPos(&mut cursor) == 0 {
            return;
        }

        SetForegroundWindow(window);

        TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
            cursor.x,
            cursor.y,
            0,
            window,
            null(),
        );
    }
}

unsafe fn remove_tray_icon(window: HWND) {
    let mut tray_data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };

    tray_data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;

    tray_data.hWnd = window;
    tray_data.uID = TRAY_ID;

    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &tray_data);
    }

    let menu: HMENU = MENU_HANDLE.swap(0, Ordering::SeqCst) as HMENU;

    if !menu.is_null() {
        unsafe {
            DestroyMenu(menu);
        }
    }
}

fn append_menu_item(menu: HMENU, id: usize, label: &str) -> Result<(), &'static str> {
    let label = wide(label);

    let result = unsafe { AppendMenuW(menu, MF_STRING, id, label.as_ptr()) };

    if result == 0 {
        return Err("AppendMenuW failed");
    }

    Ok(())
}

pub fn request_reload() {
    send_command(TrayCommand::Reload);
}

fn send_command(command: TrayCommand) {
    if let Some(sender) = COMMAND_SENDER.get() {
        let _ = sender.send(command);
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn copy_tooltip(destination: &mut [u16], text: &str) {
    let maximum_length = destination.len().saturating_sub(1);

    for (destination, character) in destination
        .iter_mut()
        .take(maximum_length)
        .zip(text.encode_utf16())
    {
        *destination = character;
    }
}

pub fn shutdown() {
    let window = TRAY_WINDOW.load(Ordering::SeqCst) as HWND;
    if !window.is_null() {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(window, WM_CLOSE, 0, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn native_tray_statistics_smoke() {
        use std::{
            sync::{Arc, Mutex, mpsc},
            time::Duration,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetDlgItem, GetWindowTextW, IsWindow, SendMessageW,
        };
        let (sender, receiver) = mpsc::channel();
        let hidden = Arc::new(AtomicBool::new(false));
        start(
            sender,
            Arc::new(Mutex::new(crate::settings::Settings::default())),
            hidden.clone(),
        );
        unsafe {
            let mut tray = null_mut();
            for _ in 0..100 {
                tray = TRAY_WINDOW.load(Ordering::SeqCst) as HWND;
                if !tray.is_null() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            assert!(!tray.is_null());
            for (index, expected) in [false, true, false, true, false].into_iter().enumerate() {
                if index > 0 {
                    SendMessageW(tray, WM_COMMAND, MENU_VISIBILITY, 0);
                }
                assert_eq!(hidden.load(Ordering::SeqCst), expected);
                assert_eq!(
                    SendMessageW(tray, TEST_VISIBILITY, 0, 0),
                    isize::from(expected)
                );
                let mut label = [0u16; 64];
                let length = windows_sys::Win32::UI::WindowsAndMessaging::GetMenuStringW(
                    MENU_HANDLE.load(Ordering::SeqCst) as HMENU,
                    MENU_VISIBILITY as u32,
                    label.as_mut_ptr(),
                    label.len() as i32,
                    MF_BYCOMMAND,
                );
                assert_eq!(
                    String::from_utf16_lossy(&label[..length as usize]),
                    visibility_label(expected)
                );
                // Simulate Explorer's re-registration request without restarting it.
                SendMessageW(
                    tray,
                    RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
                    0,
                    0,
                );
                assert_eq!(
                    SendMessageW(tray, TEST_VISIBILITY, 0, 0),
                    isize::from(expected)
                );
            }
            SendMessageW(tray, WM_COMMAND, MENU_STATISTICS, 0);
            let statistics = crate::tests::find_own_window("DatStatistics");
            assert!(!statistics.is_null());
            SendMessageW(tray, WM_COMMAND, MENU_SETTINGS, 0);
            let settings = crate::tests::find_own_window("DatSettings");
            assert!(!settings.is_null());
            let menu = MENU_HANDLE.load(Ordering::SeqCst) as HMENU;
            assert_eq!(
                windows_sys::Win32::UI::WindowsAndMessaging::GetMenuState(menu, 1002, MF_BYCOMMAND),
                u32::MAX
            );
            SendMessageW(settings, WM_COMMAND, 110, 0);
            assert!(matches!(
                receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
                TrayCommand::Reload
            ));
            SendMessageW(settings, WM_COMMAND, 107, 0); // Open Statistics
            assert_eq!(statistics, crate::tests::find_own_window("DatStatistics"));
            SendMessageW(statistics, WM_COMMAND, 202, 0); // Refresh
            let mut report = [0u16; 2048];
            assert!(
                GetWindowTextW(
                    GetDlgItem(statistics, 201),
                    report.as_mut_ptr(),
                    report.len() as i32
                ) > 0
            );
            SendMessageW(tray, WM_CLOSE, 0, 0);
            assert_eq!(IsWindow(statistics), 0);
            assert_eq!(IsWindow(settings), 0);
        }
    }
}
