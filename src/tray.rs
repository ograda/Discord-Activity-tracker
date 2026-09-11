use std::{
    mem::size_of,
    ptr::{null, null_mut},
    sync::{
        OnceLock,
        atomic::{AtomicIsize, Ordering},
        mpsc::Sender,
    },
    thread,
};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
            DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, HMENU, IDI_APPLICATION,
            LoadIconW, MF_SEPARATOR, MF_STRING, MSG, PostQuitMessage, RegisterClassW,
            SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
            TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_RBUTTONUP, WNDCLASSW,
        },
    },
};

const TRAY_ID: u32 = 1;
const TRAY_CALLBACK: u32 = WM_APP + 1;

const MENU_VISIBILITY: usize = 1001;
const MENU_RELOAD: usize = 1002;
const MENU_EXIT: usize = 1003;

static COMMAND_SENDER: OnceLock<Sender<TrayCommand>> = OnceLock::new();

static MENU_HANDLE: AtomicIsize = AtomicIsize::new(0);

#[derive(Debug, Clone, Copy)]
pub enum TrayCommand {
    ToggleHidden,
    Reload,
    Exit,
}

pub fn start(sender: Sender<TrayCommand>) {
    if COMMAND_SENDER.set(sender).is_err() {
        eprintln!("Tray command sender was already initialized.");
        return;
    }

    thread::spawn(|| {
        if let Err(error) = run_tray() {
            eprintln!("Tray error: {error}");
        }
    });
}

fn run_tray() -> Result<(), &'static str> {
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

        append_menu_item(menu, MENU_VISIBILITY, "Hide/Show activities")?;
        append_menu_item(menu, MENU_RELOAD, "Reload activities.xml")?;

        AppendMenuW(menu, MF_SEPARATOR, 0, null());

        append_menu_item(menu, MENU_EXIT, "Exit")?;

        MENU_HANDLE.store(menu as isize, Ordering::SeqCst);

        let mut tray_data: NOTIFYICONDATAW = std::mem::zeroed();

        tray_data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;

        tray_data.hWnd = window;
        tray_data.uID = TRAY_ID;

        tray_data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;

        tray_data.uCallbackMessage = TRAY_CALLBACK;

        tray_data.hIcon = LoadIconW(null_mut(), IDI_APPLICATION);

        copy_tooltip(&mut tray_data.szTip, "Discord Activity Tracker");

        if Shell_NotifyIconW(NIM_ADD, &tray_data) == 0 {
            DestroyMenu(menu);
            DestroyWindow(window);
            return Err("Shell_NotifyIconW failed");
        }

        let mut message: MSG = std::mem::zeroed();

        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
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
    match message {
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
                    send_command(TrayCommand::ToggleHidden);
                }

                MENU_RELOAD => {
                    send_command(TrayCommand::Reload);
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

        WM_CLOSE => {
            unsafe {
                DestroyWindow(window);
            }

            0
        }

        WM_DESTROY => {
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
