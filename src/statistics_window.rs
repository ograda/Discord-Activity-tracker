//! Native read-only statistics viewer on the tray UI thread.
use crate::{
    settings::data_directory,
    statistics_scope::{StatisticsScope, current_run, read_scoped_statistics},
    storage::Statistics,
};
use std::{
    cell::Cell,
    path::PathBuf,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{BeginPaint, DT_LEFT, EndPaint, InvalidateRect},
    System::LibraryLoader::GetModuleHandleW,
    UI::{Controls::EM_SETLIMITTEXT, WindowsAndMessaging::*},
};
thread_local! { static WINDOW: Cell<HWND> = const { Cell::new(null_mut()) }; }
const REPORT: i32 = 201;
const REMINDERS: i32 = 203;
const SESSIONS: i32 = 204;
const REPORTS: [i32; 3] = [REPORT, REMINDERS, SESSIONS];
const REFRESH: usize = 202;
const CLOSE: usize = 2;
const VIEW: [i32; 3] = [206, 207, 208];
const BACK: usize = 209;
const PREVIOUS: usize = 210;
const NEXT: usize = 211;
const EXPORT: usize = 212;
const EXPORT_TIMER: usize = 213;
const REFRESH_TIMER: usize = 214;
const SCOPE: i32 = 215;
struct Context {
    path: Option<PathBuf>,
    theme: crate::ui_theme::Theme,
    stats: Option<Statistics>,
    error: String,
    detail: Option<usize>,
    page: usize,
    rich_report: crate::ui_theme::RichReport,
    export_result: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    export_status: String,
    scope: StatisticsScope,
    refresh_result: Option<std::sync::mpsc::Receiver<Result<Statistics, String>>>,
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
            let context = &mut *(GetWindowLongPtrW(existing, GWLP_USERDATA) as *mut Context);
            context.detail = None;
            context.page = 0;
            resize(existing);
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
            hbrBackground: null_mut(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let window = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class.as_ptr(),
            wide("DAT Statistics \u{2014} made by Julio Ograda").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1160,
            760,
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
            Box::into_raw(Box::new(Context {
                path,
                theme: crate::ui_theme::Theme::new(),
                rich_report: crate::ui_theme::RichReport::load(),
                stats: None,
                error: String::new(),
                detail: None,
                page: 0,
                export_result: None,
                export_status: String::new(),
                scope: StatisticsScope::Total,
                refresh_result: None,
            })) as isize,
        );
        let context = &*(GetWindowLongPtrW(window, GWLP_USERDATA) as *const Context);
        context.theme.window(window);
        for (class, label, style, id) in [
            (
                context.rich_report.class(),
                "",
                WS_TABSTOP
                    | ES_MULTILINE as u32
                    | ES_READONLY as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_AUTOHSCROLL as u32,
                REPORT,
            ),
            (
                context.rich_report.class(),
                "",
                WS_TABSTOP
                    | ES_MULTILINE as u32
                    | ES_READONLY as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_AUTOHSCROLL as u32,
                REMINDERS,
            ),
            (
                context.rich_report.class(),
                "",
                WS_TABSTOP
                    | ES_MULTILINE as u32
                    | ES_READONLY as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_AUTOHSCROLL as u32,
                SESSIONS,
            ),
            (
                "BUTTON",
                "Refresh",
                WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
                REFRESH as i32,
            ),
            (
                "COMBOBOX",
                "",
                WS_TABSTOP
                    | WS_VSCROLL
                    | CBS_DROPDOWNLIST as u32
                    | CBS_OWNERDRAWFIXED as u32
                    | CBS_HASSTRINGS as u32,
                SCOPE,
            ),
            ("BUTTON", "Export JSON", WS_TABSTOP, EXPORT as i32),
            ("BUTTON", "Close", WS_TABSTOP, CLOSE as i32),
            ("BUTTON", "View all applications", WS_TABSTOP, VIEW[0]),
            ("BUTTON", "View reminder history", WS_TABSTOP, VIEW[1]),
            ("BUTTON", "View all sessions", WS_TABSTOP, VIEW[2]),
            ("BUTTON", "Back to overview", WS_TABSTOP, BACK as i32),
            ("BUTTON", "Previous", WS_TABSTOP, PREVIOUS as i32),
            ("BUTTON", "Next", WS_TABSTOP, NEXT as i32),
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
            context.theme.control(child, class, false);
            if REPORTS.contains(&id) {
                SendMessageW(child, WM_SETFONT, context.theme.body as usize, 1);
            }
        }
        for id in REPORTS {
            SendDlgItemMessageW(window, id, EM_SETLIMITTEXT, 0, 0);
        }
        for label in ["Current run", "Today", "This week", "Total"] {
            SendDlgItemMessageW(
                window,
                SCOPE,
                CB_ADDSTRING,
                0,
                wide(label).as_ptr() as isize,
            );
        }
        SendDlgItemMessageW(window, SCOPE, CB_SETCURSEL, 3, 0);
        SendDlgItemMessageW(window, SCOPE, CB_SETITEMHEIGHT, usize::MAX, 26);
        SendDlgItemMessageW(window, SCOPE, CB_SETITEMHEIGHT, 0, 26);
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
pub fn palette_changed() {
    WINDOW.with(|slot| {
        if !slot.get().is_null() {
            unsafe {
                PostMessageW(slot.get(), crate::ui_palette::CHANGED, 0, 0);
            }
        }
    });
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
    let context = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *const Context };
    if !context.is_null() {
        // Rich Edit owns its per-cell colors; generic control colors override them.
        let report_color = matches!(message, WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT)
            && REPORTS.contains(&unsafe { GetDlgCtrlID(lparam as HWND) });
        if !report_color
            && let Some(result) = unsafe { &*context }.theme.message(message, wparam, lparam)
        {
            return result;
        }
        if message == WM_PAINT {
            paint(window, unsafe { &*context });
            return 0;
        }
    }
    match message {
        crate::ui_palette::CHANGED => {
            if !context.is_null() {
                unsafe {
                    (&mut *(context as *mut Context)).theme.refresh(window);
                }
                refresh(window);
            }
            0
        }
        WM_SETTINGCHANGE => {
            crate::ui_palette::system_changed();
            0
        }
        WM_ERASEBKGND => 1,
        WM_MEASUREITEM => {
            unsafe {
                (*(lparam as *mut windows_sys::Win32::UI::Controls::MEASUREITEMSTRUCT))
                    .itemHeight = 26;
            }
            1
        }
        WM_GETMINMAXINFO => {
            unsafe {
                let info = &mut *(lparam as *mut MINMAXINFO);
                info.ptMinTrackSize.x = 1100;
                info.ptMinTrackSize.y = 680;
            }
            0
        }
        WM_COMMAND
            if VIEW.contains(&((wparam & 0xffff) as i32))
                || matches!(wparam & 0xffff, BACK | PREVIOUS | NEXT) =>
        {
            if !context.is_null() {
                unsafe {
                    let context = &mut *(context as *mut Context);
                    match wparam & 0xffff {
                        BACK => {
                            context.detail = None;
                            context.page = 0;
                        }
                        PREVIOUS => context.page = context.page.saturating_sub(1),
                        NEXT => context.page = context.page.saturating_add(1),
                        id => {
                            context.detail = VIEW.iter().position(|&v| v as usize == id);
                            context.page = 0;
                        }
                    }
                }
                resize(window);
                unsafe {
                    let id = if wparam & 0xffff == BACK {
                        VIEW[0]
                    } else if VIEW.contains(&((wparam & 0xffff) as i32)) {
                        BACK as i32
                    } else {
                        (wparam & 0xffff) as i32
                    };
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(GetDlgItem(
                        window, id,
                    ));
                }
            }
            0
        }
        WM_COMMAND
            if wparam & 0xffff == SCOPE as usize && (wparam >> 16) == CBN_SELCHANGE as usize =>
        {
            if !context.is_null() {
                unsafe {
                    let context = &mut *(context as *mut Context);
                    let index = SendDlgItemMessageW(window, SCOPE, CB_GETCURSEL, 0, 0) as usize;
                    context.scope = StatisticsScope::ALL.get(index).copied().unwrap_or_default();
                    context.page = 0;
                }
                refresh(window);
            }
            0
        }
        WM_TIMER if wparam == REFRESH_TIMER => {
            poll_refresh(window);
            0
        }
        WM_COMMAND if wparam & 0xffff == EXPORT => {
            begin_export(window);
            0
        }
        WM_TIMER if wparam == EXPORT_TIMER => {
            poll_export(window);
            0
        }
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
fn save_export_path(window: HWND) -> anyhow::Result<Option<(PathBuf, bool)>> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::UI::Controls::Dialogs::*;
    let default = crate::statistics_export::filename(crate::storage::wall_time())?;
    let mut filename = vec![0u16; 32768];
    let initial = wide(&default);
    filename[..initial.len()].copy_from_slice(&initial);
    let filter = wide("JSON files (*.json)\0*.json\0All files (*.*)\0*.*\0");
    let extension = wide("json");
    let title = wide("Export DAT Statistics");
    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = window;
    dialog.lpstrFile = filename.as_mut_ptr();
    dialog.nMaxFile = filename.len() as u32;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.nFilterIndex = 1;
    dialog.lpstrDefExt = extension.as_ptr();
    dialog.lpstrTitle = title.as_ptr();
    dialog.Flags = OFN_EXPLORER | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    if unsafe { GetSaveFileNameW(&mut dialog) } == 0 {
        let error = unsafe { CommDlgExtendedError() };
        if error == 0 {
            return Ok(None);
        }
        anyhow::bail!("Windows Save As dialog failed (code {error:#x})");
    }
    let length = filename
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(filename.len());
    let path = PathBuf::from(std::ffi::OsString::from_wide(&filename[..length]));
    let replace = path.try_exists()?;
    // Confirm after resolving the actual returned filename, then pass explicit
    // replacement permission to the writer. If absent, a racing new file is safe.
    if replace
        && unsafe {
            MessageBoxW(
                window,
                wide(&format!("{} already exists. Replace it?", path.display())).as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
            )
        } != IDYES
    {
        return Ok(None);
    }
    Ok(Some((path, replace)))
}

fn export_error(window: HWND, error: &str) {
    unsafe {
        let ptr = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
        if ptr.is_null() {
            return;
        }
        let context = &mut *ptr;
        crate::storage::Storage::new(context.path.clone())
            .log_error(&format!("Statistics export failed: {error}"));
        context.export_status = "Statistics export failed. Try Export JSON again.".into();
        InvalidateRect(window, null(), 0);
        MessageBoxW(
            window,
            wide(error).as_ptr(),
            wide("DAT Statistics — Export JSON").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn begin_export(window: HWND) {
    unsafe {
        let ptr = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
        if ptr.is_null() || (*ptr).export_result.is_some() {
            return;
        }
        let database = (*ptr).path.clone();
        let selection = match save_export_path(window) {
            Ok(Some(selection)) => selection,
            Ok(None) => return,
            Err(error) => {
                export_error(window, &format!("{error:#}"));
                return;
            }
        };
        let Some(database) = database else {
            export_error(window, "Local statistics database unavailable.");
            return;
        };
        let ptr = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
        if ptr.is_null() {
            return;
        }
        // A timer polls a channel: workers never retain an HWND or context pointer.
        // Closing/reopening Statistics during export is safe.
        let (sender, receiver) = std::sync::mpsc::channel();
        if SetTimer(window, EXPORT_TIMER, 100, None) == 0 {
            export_error(window, "Could not start export completion notification.");
            return;
        }
        let log_path = database.clone();
        let spawn = std::thread::Builder::new()
            .name("statistics-export".into())
            .spawn(move || {
                let result = crate::statistics_export::checkpoint().and_then(|()| {
                    crate::statistics_export::export(&database, &selection.0, selection.1)
                });
                if let Err(error) = &result {
                    crate::storage::Storage::new(Some(log_path))
                        .log_error(&format!("Statistics export failed: {error:#}"));
                }
                let _ = sender.send(result.map_err(|error| format!("{error:#}")));
            });
        if let Err(error) = spawn {
            KillTimer(window, EXPORT_TIMER);
            export_error(
                window,
                &format!("Could not start statistics export: {error}"),
            );
            return;
        }
        (*ptr).export_result = Some(receiver);
        (*ptr).export_status = "Exporting statistics locally…".into();
        windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
            GetDlgItem(window, EXPORT as i32),
            0,
        );
        InvalidateRect(window, null(), 0);
    }
}

fn poll_export(window: HWND) {
    unsafe {
        let ptr = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
        if ptr.is_null() {
            return;
        }
        let Some(receiver) = &(*ptr).export_result else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("Statistics export worker stopped unexpectedly.".into())
            }
        };
        (*ptr).export_result = None;
        KillTimer(window, EXPORT_TIMER);
        windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
            GetDlgItem(window, EXPORT as i32),
            1,
        );
        match result {
            Ok(()) => (*ptr).export_status = "Statistics exported successfully.".into(),
            Err(error) => export_error(window, &error),
        }
        InvalidateRect(window, null(), 0);
    }
}

fn resize(window: HWND) {
    unsafe {
        let ptr = GetWindowLongPtrW(window, GWLP_USERDATA) as *const Context;
        if ptr.is_null() {
            return;
        }
        let detail = (*ptr).detail;
        let mut bounds: RECT = std::mem::zeroed();
        GetClientRect(window, &mut bounds);
        MoveWindow(
            GetDlgItem(window, SCOPE),
            bounds.right - 212,
            28,
            188,
            160,
            1,
        );
        let layout = panels(bounds.right, bounds.bottom);
        for (index, id) in REPORTS.into_iter().enumerate() {
            let visible = detail.is_none_or(|selected| selected == index);
            ShowWindow(
                GetDlgItem(window, id),
                if visible { SW_SHOWNA } else { SW_HIDE },
            );
            ShowWindow(
                GetDlgItem(window, VIEW[index]),
                if detail.is_none() { SW_SHOWNA } else { SW_HIDE },
            );
            let panel = if detail.is_some() {
                detail_panel(bounds.right, bounds.bottom)
            } else {
                layout[index]
            };
            MoveWindow(
                GetDlgItem(window, id),
                panel.left + 16,
                panel.top + 62,
                panel.right - panel.left - 32,
                table_height(panel, detail.is_some()),
                1,
            );
            MoveWindow(
                GetDlgItem(window, VIEW[index]),
                (panel.left + panel.right - 194) / 2,
                panel.bottom - 46,
                194,
                32,
                1,
            );
        }
        for id in [BACK, PREVIOUS, NEXT] {
            ShowWindow(
                GetDlgItem(window, id as i32),
                if detail.is_some() { SW_SHOWNA } else { SW_HIDE },
            );
        }
        MoveWindow(GetDlgItem(window, BACK as i32), 24, 100, 166, 32, 1);
        let panel = detail_panel(bounds.right, bounds.bottom);
        MoveWindow(
            GetDlgItem(window, PREVIOUS as i32),
            bounds.right / 2 - 174,
            panel.bottom - 44,
            100,
            30,
            1,
        );
        MoveWindow(
            GetDlgItem(window, NEXT as i32),
            bounds.right / 2 + 74,
            panel.bottom - 44,
            100,
            30,
            1,
        );
        for (id, offset, width) in [(REFRESH, 0, 108), (EXPORT, 120, 132), (CLOSE, 264, 108)] {
            MoveWindow(
                GetDlgItem(window, id as i32),
                (bounds.right - 372) / 2 + offset,
                bounds.bottom - 70,
                width,
                32,
                1,
            );
        }
        render_tables(window);
        InvalidateRect(window, null(), 0);
    }
}
fn detail_panel(width: i32, height: i32) -> RECT {
    crate::ui_theme::rect(24, 144, width - 24, height - 90)
}
fn table_height(panel: RECT, detail: bool) -> i32 {
    (panel.bottom - panel.top - if detail { 180 } else { 116 }).max(60)
}
fn row_capacity(height: i32) -> usize {
    ((height - 8) / 28 - 1).max(1) as usize
}
fn page_bounds(
    total: usize,
    capacity: usize,
    page: usize,
) -> (usize, usize, std::ops::Range<usize>) {
    let capacity = capacity.max(1);
    let pages = total.div_ceil(capacity).max(1);
    let page = page.min(pages - 1);
    let start = (page * capacity).min(total);
    (page, pages, start..(start + capacity).min(total))
}
fn refresh(window: HWND) {
    let context = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *const Context };
    if context.is_null() {
        return;
    }
    let context = unsafe { &mut *(context as *mut Context) };
    context.stats = None;
    context.refresh_result = None;
    unsafe {
        KillTimer(window, REFRESH_TIMER);
    }
    context.error.clear();
    let Some(path) = context.path.clone() else {
        context.error =
            "Statistics unavailable: the local data directory could not be located.".into();
        render_tables(window);
        return;
    };
    if crate::statistics_export::checkpoint_available() {
        context.error = "Refreshing saved statistics…".into();
        let (sender, receiver) = std::sync::mpsc::channel();
        let scope = context.scope;
        let timer = unsafe { SetTimer(window, REFRESH_TIMER, 100, None) };
        if timer == 0 {
            context.error = "Could not start Statistics refresh notification.".into();
        } else if let Err(error) = std::thread::Builder::new()
            .name("statistics-refresh".into())
            .spawn(move || {
                let result = crate::statistics_export::checkpoint().and_then(|()| {
                    read_scoped_statistics(&path, scope, current_run(), crate::storage::wall_time())
                });
                if let Err(error) = &result {
                    crate::storage::Storage::new(Some(path))
                        .log_error(&format!("Statistics refresh failed: {error:#}"));
                }
                let _ = sender.send(result.map_err(|error| format!("{error:#}")));
            })
        {
            unsafe {
                KillTimer(window, REFRESH_TIMER);
            }
            context.error = format!("Could not start Statistics refresh: {error}");
        } else {
            context.refresh_result = Some(receiver);
        }
    } else {
        // Read-only previews/tests without a running tracker still work.
        match read_scoped_statistics(
            &path,
            context.scope,
            current_run(),
            crate::storage::wall_time(),
        ) {
            Ok(stats) => context.stats = Some(stats),
            Err(error) => {
                eprintln!("Could not read statistics: {error:#}");
                context.error = format!(
                    "Statistics unavailable: {error:#}\r\nTry Refresh. Tracking continues independently."
                );
            }
        }
    }
    render_tables(window);
    unsafe {
        InvalidateRect(window, null(), 0);
    }
}
fn poll_refresh(window: HWND) {
    unsafe {
        let ptr = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
        if ptr.is_null() {
            return;
        }
        let context = &mut *ptr;
        let Some(receiver) = &context.refresh_result else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("Statistics refresh stopped unexpectedly.".into())
            }
        };
        context.refresh_result = None;
        KillTimer(window, REFRESH_TIMER);
        match result {
            Ok(stats) => {
                context.stats = Some(stats);
                context.error.clear();
            }
            Err(error) => {
                context.error = format!("Statistics unavailable: {error}\r\nTry Refresh.")
            }
        }
        render_tables(window);
        InvalidateRect(window, null(), 0);
    }
}
fn panels(width: i32, height: i32) -> [RECT; 3] {
    use crate::ui_theme::rect;
    let middle = 100 + (height - 100 - 90 - 16) / 2;
    [
        rect(24, 100, width / 2 - 8, middle),
        rect(width / 2 + 8, 100, width - 24, middle),
        rect(24, middle + 16, width - 24, height - 90),
    ]
}
fn render_tables(window: HWND) {
    unsafe {
        let ptr = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
        if ptr.is_null() {
            return;
        }
        let context = &mut *ptr;
        let Some(stats) = &context.stats else {
            for id in REPORTS {
                context
                    .rich_report
                    .set_text(GetDlgItem(window, id), &context.error);
            }
            for id in [PREVIOUS, NEXT] {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    GetDlgItem(window, id as i32),
                    0,
                );
            }
            return;
        };
        for (index, mut table) in statistics_tables(stats).into_iter().enumerate() {
            if context.detail.is_some_and(|selected| selected != index) {
                continue;
            }
            let child = GetDlgItem(window, REPORTS[index]);
            let mut bounds: RECT = std::mem::zeroed();
            GetClientRect(child, &mut bounds);
            let capacity = row_capacity(bounds.bottom);
            if context.detail.is_some() {
                let (page, pages, range) = page_bounds(table.rows.len(), capacity, context.page);
                context.page = page;
                table.rows = table.rows[range].to_vec();
                for (id, enabled) in [(PREVIOUS, page > 0), (NEXT, page + 1 < pages)] {
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                        GetDlgItem(window, id as i32),
                        i32::from(enabled),
                    );
                }
            } else {
                table.rows.truncate(capacity);
                // Overview keeps the key metrics; every original column is in the detail view.
                if index == 0 {
                    table.headers.truncate(3);
                    for row in &mut table.rows {
                        row.truncate(3);
                    }
                    table.widths = vec![180, 125, 125];
                } else if index == 1 {
                    table.widths = vec![160, 82, 66, 125];
                }
            }
            let mut widths = table.widths;
            let sum: i32 = widths.iter().sum();
            widths[0] = (widths[0] + bounds.right - 12 - sum).max(120);
            let notes = if table.rows.is_empty() {
                table.notes.as_str()
            } else {
                ""
            };
            context
                .rich_report
                .set_table(child, &table.headers, &table.rows, notes, &widths);
            windows_sys::Win32::UI::Controls::ShowScrollBar(child, SB_BOTH, 0);
            SendMessageW(child, windows_sys::Win32::UI::Controls::EM_SETSEL, 0, 0);
            SendMessageW(
                child,
                windows_sys::Win32::UI::Controls::EM_SCROLLCARET,
                0,
                0,
            );
        }
    }
}
fn paint(window: HWND, context: &Context) {
    use crate::ui_theme::{draw_text, rect};
    use windows_sys::Win32::Graphics::Gdi::{
        DT_CENTER, DT_RIGHT, DT_WORDBREAK, DrawTextW, SelectObject, SetBkMode, SetTextColor,
        TRANSPARENT,
    };
    unsafe {
        let mut ps = std::mem::zeroed();
        let dc = BeginPaint(window, &mut ps);
        let mut bounds = std::mem::zeroed();
        GetClientRect(window, &mut bounds);
        let theme = &context.theme;
        theme.background(dc, &bounds);
        theme.header(dc, "DAT Statistics", bounds.right);
        let tables = context.stats.as_ref().map(statistics_tables);
        for (index, panel) in panels(bounds.right, bounds.bottom).into_iter().enumerate() {
            if context.detail.is_some_and(|selected| selected != index) {
                continue;
            }
            let panel = if context.detail.is_some() {
                detail_panel(bounds.right, bounds.bottom)
            } else {
                panel
            };
            dashboard_panel(dc, panel, theme);
            section_icon(dc, panel.left + 16, panel.top + 13, index, theme);
            let (heading, subtitle) = [
                (
                    "APPLICATION ACTIVITY",
                    "Open time, foreground time and tracked instances",
                ),
                (
                    "REMINDER STATISTICS",
                    "Completions, snoozes and response times",
                ),
                ("RECENT SESSIONS", "Latest observed sessions"),
            ][index];
            draw_text(
                dc,
                heading,
                rect(
                    panel.left + 60,
                    panel.top + 8,
                    panel.right - 16,
                    panel.top + 31,
                ),
                theme.title,
                theme.colors.accent,
                DT_LEFT,
            );
            draw_text(
                dc,
                subtitle,
                rect(
                    panel.left + 60,
                    panel.top + 31,
                    panel.right - 16,
                    panel.top + 53,
                ),
                theme.body,
                theme.colors.muted,
                DT_LEFT,
            );
            if let Some(tables) = &tables {
                let table = &tables[index];
                let capacity = row_capacity(table_height(panel, context.detail.is_some()));
                if context.detail.is_some() {
                    let (page, pages, _) = page_bounds(table.rows.len(), capacity, context.page);
                    draw_text(
                        dc,
                        &format!("Page {} of {pages}", page + 1),
                        rect(
                            bounds.right / 2 - 72,
                            panel.bottom - 44,
                            bounds.right / 2 + 72,
                            panel.bottom - 14,
                        ),
                        theme.heading,
                        theme.colors.text,
                        DT_CENTER,
                    );
                    let old = SelectObject(dc, theme.heading);
                    SetBkMode(dc, TRANSPARENT as i32);
                    SetTextColor(dc, theme.colors.muted);
                    let mut note_rect = rect(
                        panel.left + 18,
                        panel.bottom - 108,
                        panel.right - 18,
                        panel.bottom - 52,
                    );
                    let note = wide(&table.notes);
                    DrawTextW(
                        dc,
                        note.as_ptr(),
                        -1,
                        &mut note_rect,
                        DT_LEFT | DT_WORDBREAK,
                    );
                    SelectObject(dc, old);
                } else {
                    draw_text(
                        dc,
                        &format!("{} of {}", table.rows.len().min(capacity), table.rows.len()),
                        rect(
                            panel.right - 108,
                            panel.bottom - 43,
                            panel.right - 16,
                            panel.bottom - 15,
                        ),
                        theme.heading,
                        theme.colors.muted,
                        DT_RIGHT,
                    );
                }
            }
        }
        draw_text(
            dc,
            if context.export_status.is_empty() {
                "Stored locally on this device."
            } else {
                &context.export_status
            },
            rect(24, bounds.bottom - 30, bounds.right - 24, bounds.bottom - 8),
            theme.heading,
            theme.colors.muted,
            DT_CENTER,
        );
        EndPaint(window, &ps);
    }
}
struct StatisticsTable {
    headers: Vec<&'static str>,
    rows: Vec<Vec<String>>,
    widths: Vec<i32>,
    notes: String,
}
fn dashboard_panel(
    dc: windows_sys::Win32::Graphics::Gdi::HDC,
    r: RECT,
    theme: &crate::ui_theme::Theme,
) {
    let c = theme.colors;
    let blend = |a: u32, b: u32, n: u32| {
        let channel =
            |shift: u32| (((a >> shift) & 255) * (100 - n) + ((b >> shift) & 255) * n) / 100;
        channel(0) | (channel(8) << 8) | (channel(16) << 16)
    };
    unsafe {
        for (inset, strength) in [(0, 18), (1, 35), (2, 65), (3, 100)] {
            crate::ui_theme::rounded(
                dc,
                RECT {
                    left: r.left + inset,
                    top: r.top + inset,
                    right: r.right - inset,
                    bottom: r.bottom - inset,
                },
                c.panel,
                blend(c.background, c.border, strength),
            );
        }
        crate::ui_theme::rounded(
            dc,
            RECT {
                left: r.left + 4,
                top: r.top + 4,
                right: r.right - 4,
                bottom: r.bottom - 4,
            },
            c.panel,
            blend(c.panel, c.accent, 18),
        );
    }
}
fn section_icon(
    dc: windows_sys::Win32::Graphics::Gdi::HDC,
    x: i32,
    y: i32,
    index: usize,
    theme: &crate::ui_theme::Theme,
) {
    use crate::ui_theme::rect;
    use windows_sys::Win32::Graphics::Gdi::*;
    unsafe {
        crate::ui_theme::rounded(
            dc,
            rect(x, y, x + 32, y + 32),
            theme.colors.input,
            theme.colors.border,
        );
        let pen = CreatePen(PS_SOLID, 2, theme.colors.accent);
        let old_pen = SelectObject(dc, pen);
        let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
        match index {
            0 => {
                Rectangle(dc, x + 7, y + 7, x + 25, y + 21);
                MoveToEx(dc, x + 16, y + 21, null_mut());
                LineTo(dc, x + 16, y + 25);
                MoveToEx(dc, x + 11, y + 25, null_mut());
                LineTo(dc, x + 22, y + 25);
            }
            1 => {
                RoundRect(dc, x + 9, y + 7, x + 23, y + 23, 10, 10);
                MoveToEx(dc, x + 6, y + 23, null_mut());
                LineTo(dc, x + 26, y + 23);
                MoveToEx(dc, x + 14, y + 26, null_mut());
                LineTo(dc, x + 19, y + 26);
            }
            _ => {
                Ellipse(dc, x + 6, y + 6, x + 27, y + 27);
                MoveToEx(dc, x + 16, y + 10, null_mut());
                LineTo(dc, x + 16, y + 17);
                LineTo(dc, x + 22, y + 17);
            }
        }
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        DeleteObject(pen);
    }
}
fn statistics_tables(stats: &Statistics) -> [StatisticsTable; 3] {
    let missing = |value: Option<i64>| value.map(|v| v.to_string()).unwrap_or_else(|| "--".into());
    let activities = StatisticsTable {
        headers: vec![
            "Activity",
            "Total / Open",
            "Active",
            "Sessions",
            "Events",
            "Windows",
            "Processes",
        ],
        widths: vec![230, 145, 145, 75, 65, 75, 80],
        rows: stats
            .activities
            .iter()
            .map(|row| {
                vec![
                    if row.session_totals_only {
                        format!("{} (sessions only)", row.name)
                    } else {
                        row.name.clone()
                    },
                    duration(row.open_ms),
                    duration(row.active_ms),
                    row.sessions.to_string(),
                    row.events.to_string(),
                    missing(row.peak_window_instances),
                    missing(row.peak_processes),
                ]
            })
            .collect(),
        notes: if stats.activities.is_empty() {
            "No saved activity data yet. Enable cumulative time or session history in Settings."
                .into()
        } else {
            "Times cover saved collection periods in the selected scope. Range/run times use new observed intervals; legacy totals remain in Total. Sessions/events belong to the scope. Windows/processes are recorded peaks; -- means unavailable.".into()
        },
    };
    let reminders = StatisticsTable {
        headers: vec!["Reminder", "Completed", "Snoozes", "Average response"],
        widths: vec![300, 125, 125, 200],
        rows: stats
            .reminders
            .iter()
            .map(|row| {
                vec![
                    row.name.clone(),
                    row.completed.to_string(),
                    row.snoozes.to_string(),
                    row.average_response_ms
                        .map(duration)
                        .unwrap_or_else(|| "No completions".into()),
                ]
            })
            .collect(),
        notes: if stats.reminders.is_empty() {
            "No detailed reminder data yet. Enable detailed statistics in Settings to collect new occurrences.".into()
        } else {
            "Counts use the selected scope; date scopes count snoozes by event time. Average response uses completed reminders in the scope.".into()
        },
    };
    let sessions = StatisticsTable {
        headers: vec!["Activity", "Last observed session (UTC)"],
        widths: vec![380, 370],
        rows: stats
            .activities
            .iter()
            .map(|row| {
                vec![
                    row.name.clone(),
                    row.last_session
                        .clone()
                        .unwrap_or_else(|| "No sessions recorded".into()),
                ]
            })
            .collect(),
        notes: if stats.activities.is_empty() {
            "No sessions recorded yet.".into()
        } else {
            "Latest observed session in the selected scope. Refresh checkpoints pending data without closing live sessions.".into()
        },
    };
    [activities, reminders, sessions]
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
pub(crate) fn preview_data(window: HWND) {
    use crate::storage::{ActivitySummary, ReminderSummary};
    let stats = Statistics {
        activities: ["World of Warcraft", "Visual Studio Code", "Google Chrome"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| ActivitySummary {
                name: name.into(),
                open_ms: 12_345_000 + index as u64 * 5_000_000,
                active_ms: 7_123_000,
                sessions: 12,
                events: 24,
                peak_processes: Some(14),
                peak_window_instances: Some(2),
                last_session: Some("2026-10-11 14:32:00".into()),
                session_totals_only: false,
            })
            .collect(),
        reminders: vec![ReminderSummary {
            name: "Drink water".into(),
            completed: 24,
            snoozes: 3,
            average_response_ms: Some(42_000),
        }],
    };
    unsafe {
        let context = &mut *(GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context);
        context.stats = Some(stats);
        render_tables(window);
        InvalidateRect(window, null(), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_state_and_duration() {
        let tables = statistics_tables(&Statistics::default());
        assert!(tables[0].notes.contains("No saved activity data"));
        assert!(tables[1].notes.contains("No detailed reminder data"));
        assert_eq!(duration(5_523_000), "1h 32m 03s");
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    #[test]
    fn pagination_keeps_every_row_and_clamps_after_resize() {
        assert_eq!(page_bounds(0, 4, 99), (0, 1, 0..0));
        assert_eq!(page_bounds(12, 4, 1), (1, 3, 4..8));
        assert_eq!(page_bounds(13, 4, 99), (3, 4, 12..13));
        let rows: Vec<_> = (0..4).flat_map(|page| page_bounds(13, 4, page).2).collect();
        assert_eq!(rows, (0..13).collect::<Vec<_>>());
        assert_eq!(page_bounds(13, 20, 3), (0, 1, 0..13));
    }
    #[test]
    fn overview_panels_and_complete_rows_fit_minimum_size() {
        for (width, height) in [(1084, 641), (1144, 721), (1400, 900)] {
            let cards = panels(width, height);
            assert!(cards[0].right < cards[1].left);
            assert_eq!(cards[0].top, cards[1].top);
            assert!(cards[2].top > cards[0].bottom);
            for card in cards {
                let height = table_height(card, false);
                assert!((row_capacity(height) + 1) * 28 + 8 <= height as usize);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn preview_many_rows(window: HWND) {
    unsafe {
        let context = &mut *(GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context);
        let stats = context.stats.as_mut().unwrap();
        stats.activities = (0..25)
            .map(|index| crate::storage::ActivitySummary {
                name: format!("Application {index:02}"),
                open_ms: 1000,
                active_ms: 500,
                sessions: 1,
                events: 2,
                peak_processes: Some(2),
                peak_window_instances: Some(1),
                last_session: Some("2026-10-11 14:32:00".into()),
                session_totals_only: false,
            })
            .collect();
        resize(window);
    }
}
