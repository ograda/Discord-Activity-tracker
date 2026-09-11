use std::{
    mem::size_of,
    ptr::{null, null_mut},
    thread,
};

use windows_sys::Win32::{
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            NIF_ICON, NIF_TIP, NIM_ADD, NOTIFYICONDATAW,
            Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            CreateWindowExW, IDI_APPLICATION, LoadIconW,
        },
    },
};

pub fn start() {
    thread::spawn(|| {
        if let Err(error) = create_tray_icon() {
            eprintln!("Could not create tray icon: {error}");
        }
    });
}

fn create_tray_icon() -> Result<(), &'static str> {
    unsafe {
        let instance = GetModuleHandleW(null());

        if instance.is_null() {
            return Err("GetModuleHandleW failed");
        }

        // STATIC is a built-in Windows window class.
        // The window remains hidden because it has no visible style.
        let class_name = wide("STATIC");
        let window_name = wide("Discord Activity Tracker");

        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_name.as_ptr(),
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

        let mut tray_data: NOTIFYICONDATAW =
            std::mem::zeroed();

        tray_data.cbSize =
            size_of::<NOTIFYICONDATAW>() as u32;

        tray_data.hWnd = window;
        tray_data.uID = 1;
        tray_data.uFlags = NIF_ICON | NIF_TIP;

        tray_data.hIcon =
            LoadIconW(null_mut(), IDI_APPLICATION);

        copy_tooltip(
            &mut tray_data.szTip,
            "Discord Activity Tracker",
        );

        if Shell_NotifyIconW(NIM_ADD, &tray_data) == 0 {
            return Err("Shell_NotifyIconW failed");
        }

        // Keep the thread and its hidden window alive.
        loop {
            thread::park();
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .collect()
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