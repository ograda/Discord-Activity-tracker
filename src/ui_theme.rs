//! Shared paint resources for the two utility windows; native input stays native.
use crate::ui_palette::{Colors, current};
use std::ptr::{null, null_mut};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::{
        Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute},
        Gdi::*,
    },
    UI::{
        Controls::*,
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};
pub struct Theme {
    pub colors: Colors,
    pub body: HFONT,
    pub heading: HFONT,
    pub title: HFONT,
    pub mono: HFONT,
    background: HBRUSH,
    panel: HBRUSH,
    input: HBRUSH,
    icon: Option<crate::tray_icon::TrayIcon>,
}
impl Theme {
    pub fn new() -> Self {
        let colors = current();
        unsafe {
            Self {
                colors,
                body: font("Segoe UI", 15, 400),
                heading: font("Segoe UI", 13, 600),
                title: font("Segoe UI Semibold", 20, 600),
                mono: font("Consolas", 14, 400),
                background: CreateSolidBrush(colors.background),
                panel: CreateSolidBrush(colors.panel),
                input: CreateSolidBrush(colors.input),
                icon: crate::tray_icon::TrayIcon::load(48).ok(),
            }
        }
    }
    pub fn refresh(&mut self, window: HWND) {
        self.colors = current();
        unsafe {
            for (brush, color) in [
                (&mut self.background, self.colors.background),
                (&mut self.panel, self.colors.panel),
                (&mut self.input, self.colors.input),
            ] {
                let replacement = CreateSolidBrush(color);
                if !replacement.is_null() {
                    DeleteObject(*brush as _);
                    *brush = replacement;
                }
            }
            self.window(window);
            RedrawWindow(window, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
    }
    pub fn window(&self, window: HWND) {
        unsafe {
            let dark: i32 = i32::from(self.colors.dark);
            // Unsupported on older Windows: the standard title bar remains usable.
            DwmSetWindowAttribute(
                window,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                &dark as *const _ as _,
                4,
            );
            if let Some(icon) = &self.icon {
                SendMessageW(window, WM_SETICON, ICON_SMALL as usize, icon.0 as isize);
                SendMessageW(window, WM_SETICON, ICON_BIG as usize, icon.0 as isize);
            }
        }
    }
    // Theme is stored in the parent's boxed context before any subclass is installed.
    // Child windows are destroyed before that context is released at WM_NCDESTROY.
    pub fn control(&self, control: HWND, class: &str, heading: bool) {
        unsafe {
            SendMessageW(
                control,
                WM_SETFONT,
                if heading { self.heading } else { self.body } as usize,
                1,
            );
            SetWindowTheme(control, wide("").as_ptr(), wide("").as_ptr());
            if class == "COMBOBOX" && SetWindowSubclass(control, Some(combo_proc), 2, 0) == 0 {
                eprintln!("Could not apply DAT dropdown styling");
            }
            if class == "BUTTON" {
                let style = GetWindowLongPtrW(control, GWL_STYLE) as u32;
                let state = Box::new(ButtonState {
                    checkbox: style & BS_TYPEMASK as u32 == BS_AUTOCHECKBOX as u32,
                    checked: false,
                    hover: false,
                });
                let data = Box::into_raw(state);
                if SetWindowSubclass(control, Some(button_proc), 1, data as usize) == 0 {
                    drop(Box::from_raw(data));
                    eprintln!("Could not apply DAT button styling");
                } else if SetPropW(control, wide("DatButtonState").as_ptr(), data as _) == 0 {
                    RemoveWindowSubclass(control, Some(button_proc), 1);
                    drop(Box::from_raw(data));
                    eprintln!("Could not retain DAT button state");
                } else {
                    SetWindowLongPtrW(
                        control,
                        GWL_STYLE,
                        ((style & !(BS_TYPEMASK as u32)) | BS_OWNERDRAW as u32) as isize,
                    );
                }
            }
        }
    }
    pub fn message(&self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        unsafe {
            match message {
                WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
                    let dc = wparam as HDC;
                    let child = lparam as HWND;
                    let id = GetDlgCtrlID(child);
                    let footer = id == 304;
                    let input =
                        message == WM_CTLCOLOREDIT || message == WM_CTLCOLORLISTBOX || id == 201;
                    SetBkMode(dc, TRANSPARENT as i32);
                    SetTextColor(
                        dc,
                        if (301..=303).contains(&id) || id == 305 || id >= 1000 {
                            self.colors.accent
                        } else if id == 0 || footer {
                            self.colors.muted
                        } else {
                            self.colors.text
                        },
                    );
                    SetBkColor(
                        dc,
                        if input {
                            self.colors.input
                        } else if footer {
                            self.colors.background
                        } else {
                            self.colors.panel
                        },
                    );
                    Some(if input {
                        self.input
                    } else if footer {
                        self.background
                    } else {
                        self.panel
                    } as isize)
                }
                WM_COMMAND if wparam >> 16 == BN_CLICKED as usize && lparam != 0 => {
                    if let Some(button) = button_state(lparam as HWND) {
                        let button = &mut *button;
                        if button.checkbox {
                            button.checked = !button.checked;
                            InvalidateRect(lparam as HWND, null(), 0);
                            return Some(0);
                        }
                    }
                    None
                }
                WM_DRAWITEM => {
                    let item = &*(lparam as *const DRAWITEMSTRUCT);
                    if item.CtlType == ODT_BUTTON {
                        draw_button(self, item);
                        return Some(1);
                    }
                    if item.CtlType != ODT_COMBOBOX {
                        return None;
                    }
                    FillRect(item.hDC, &item.rcItem, self.input);
                    if item.itemID != u32::MAX {
                        let length =
                            SendMessageW(item.hwndItem, CB_GETLBTEXTLEN, item.itemID as usize, 0);
                        if length >= 0 {
                            let mut text = vec![0u16; length as usize + 1];
                            SendMessageW(
                                item.hwndItem,
                                CB_GETLBTEXT,
                                item.itemID as usize,
                                text.as_mut_ptr() as isize,
                            );
                            let text = String::from_utf16_lossy(&text[..length as usize]);
                            let mut rect = item.rcItem;
                            rect.left += 10;
                            draw_text(
                                item.hDC,
                                &text,
                                rect,
                                self.body,
                                if item.itemState & ODS_SELECTED != 0 {
                                    self.colors.accent
                                } else {
                                    self.colors.text
                                },
                                DT_LEFT,
                            );
                        }
                    }
                    if item.itemState & ODS_FOCUS != 0 {
                        DrawFocusRect(item.hDC, &item.rcItem);
                    }
                    Some(1)
                }
                _ => None,
            }
        }
    }
    pub fn background(&self, dc: HDC, rect: &RECT) {
        unsafe {
            FillRect(dc, rect, self.background);
        }
    }
    pub fn header(&self, dc: HDC, title: &str, width: i32) {
        unsafe {
            if let Some(icon) = &self.icon {
                DrawIconEx(dc, 24, 19, icon.0, 48, 48, 0, null_mut(), DI_NORMAL);
            }
            draw_text(
                dc,
                title,
                rect(86, 16, width - 24, 47),
                self.title,
                self.colors.text,
                DT_LEFT,
            );
            draw_text(
                dc,
                "DISCORD ACTIVITY TRACKER",
                rect(87, 48, width - 24, 70),
                self.heading,
                self.colors.muted,
                DT_LEFT,
            );
            fill(dc, rect(24, 83, width - 24, 84), self.colors.border);
            fill(dc, rect(24, 83, 130, 85), self.colors.accent);
        }
    }
    pub fn card(&self, dc: HDC, r: RECT) {
        unsafe {
            let shadow = RECT {
                left: r.left,
                top: r.top + 3,
                right: r.right,
                bottom: r.bottom + 3,
            };
            rounded(dc, shadow, self.colors.background, self.colors.background);
            rounded(dc, r, self.colors.panel, self.colors.border);
            fill(
                dc,
                rect(r.left + 18, r.top + 1, r.left + 70, r.top + 2),
                self.colors.border,
            );
        }
    }
}

// Windows' built-in Rich Edit adds text hierarchy without a GUI dependency.
// These richedit.h declarations are not exposed by windows-sys 0.61.
#[repr(C)]
struct CharFormat {
    size: u32,
    mask: u32,
    effects: u32,
    height: i32,
    offset: i32,
    color: u32,
    charset: u8,
    pitch: u8,
    face: [u16; 32],
}
// EDITSTREAM uses four-byte packing in the Windows SDK, including on x64.
#[repr(C, packed(4))]
struct EditStream {
    cookie: usize,
    error: u32,
    callback: Option<unsafe extern "system" fn(usize, *mut u8, i32, *mut i32) -> u32>,
}
unsafe extern "system" fn stream_rtf(
    cookie: usize,
    buffer: *mut u8,
    size: i32,
    read: *mut i32,
) -> u32 {
    unsafe {
        let remaining = &mut *(cookie as *mut &[u8]);
        let count = remaining.len().min(size.max(0) as usize);
        std::ptr::copy_nonoverlapping(remaining.as_ptr(), buffer, count);
        *remaining = &remaining[count..];
        *read = count as i32;
    }
    0
}
fn rtf_escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.encode_utf16() {
        match ch {
            92 | 123 | 125 => {
                out.push('\\');
                out.push(char::from_u32(ch as u32).unwrap());
            }
            10 => out.push_str("\\line "),
            13 => {}
            32..=126 => out.push(char::from_u32(ch as u32).unwrap()),
            _ => out.push_str(&format!("\\u{}?", ch as i16)),
        }
    }
    out
}
fn table_rtf(headers: &[&str], rows: &[Vec<String>], notes: &str, widths: &[i32]) -> String {
    let colors = current();
    let mut rtf = String::from("{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Segoe UI;}}{\\colortbl;");
    for color in [
        colors.text,
        colors.muted,
        colors.panel,
        colors.input,
        colors.border,
        colors.accent,
    ] {
        rtf.push_str(&format!(
            "\\red{}\\green{}\\blue{};",
            color & 255,
            (color >> 8) & 255,
            (color >> 16) & 255
        ));
    }
    rtf.push_str("}\\viewkind4\\uc1\\f0\\fs20 ");
    for (index, row) in std::iter::once(headers.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .chain(rows.iter().cloned())
        .enumerate()
    {
        rtf.push_str("\\trowd\\trgaph110\\trleft0\\trrh420");
        let mut edge = 0;
        for width in widths {
            edge += width * 15;
            let shade = if index % 2 == 0 { 4 } else { 3 };
            rtf.push_str(&format!(
                "\\clvertalc\\clcbpat{shade}\\clbrdrb\\brdrs\\brdrw5\\brdrcf5\\cellx{edge}"
            ));
        }
        for (column, cell) in row.iter().enumerate() {
            let align = if column == 0 { "ql" } else { "qr" };
            let style = if index == 0 {
                "\\b\\fs18\\cf6"
            } else {
                "\\b0\\fs20\\cf1"
            };
            rtf.push_str(&format!(
                "\\pard\\intbl\\{align} {style} {}\\cell ",
                rtf_escape(cell)
            ));
        }
        rtf.push_str("\\row ");
    }
    rtf.push_str(&format!(
        "\\pard\\sa100\\sb160\\b0\\fs18\\cf2 {}\\par}}",
        rtf_escape(notes)
    ));
    rtf
}
pub struct RichReport(windows_sys::Win32::Foundation::HMODULE);
impl RichReport {
    pub fn load() -> Self {
        use windows_sys::Win32::System::LibraryLoader::{
            LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
        };
        let module = unsafe {
            LoadLibraryExW(
                wide("Msftedit.dll").as_ptr(),
                null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            eprintln!("Rich Edit unavailable; Statistics uses the plain native report control");
        }
        Self(module)
    }
    pub fn class(&self) -> &'static str {
        if self.0.is_null() {
            "EDIT"
        } else {
            "RICHEDIT50W"
        }
    }
    pub fn set_table(
        &self,
        window: HWND,
        headers: &[&str],
        rows: &[Vec<String>],
        notes: &str,
        widths: &[i32],
    ) {
        if self.0.is_null() {
            let mut text = headers.join("\t");
            for row in rows {
                text.push_str("\r\n");
                text.push_str(&row.join("\t"));
            }
            text.push_str("\r\n\r\n");
            text.push_str(notes);
            self.set_text(window, &text);
            return;
        }
        let rtf = table_rtf(headers, rows, notes, widths);
        let mut remaining = rtf.as_bytes();
        let mut stream = EditStream {
            cookie: &mut remaining as *mut _ as usize,
            error: 0,
            callback: Some(stream_rtf),
        };
        unsafe {
            SendMessageW(window, WM_SETREDRAW, 0, 0);
            SendMessageW(window, WM_USER + 67, 0, current().panel as isize);
            SendMessageW(window, 1228, 131072, 131072); // EM_SETEDITSTYLE / SES_HIDEGRIDLINES
            SendMessageW(window, 1096, 0, 0); // EM_SETTARGETDEVICE: wrap notes to viewport
            SendMessageW(window, WM_USER + 73, 2, &mut stream as *mut _ as isize);
            if stream.error != 0 {
                eprintln!("Could not render statistics table: {}", { stream.error });
            }
            SendMessageW(window, WM_SETREDRAW, 1, 0);
            InvalidateRect(window, null(), 0);
        }
    }
    pub fn set_text(&self, window: HWND, text: &str) {
        let colors = current();
        unsafe {
            if self.0.is_null() {
                SetWindowTextW(window, wide(text).as_ptr());
                return;
            }
            const EM_SETCHARFORMAT: u32 = WM_USER + 68;
            const EM_SETBKGNDCOLOR: u32 = WM_USER + 67;
            const EM_EXSETSEL: u32 = WM_USER + 55;
            let format = |face: &str, color, height, bold| {
                let mut value = CharFormat {
                    size: std::mem::size_of::<CharFormat>() as u32,
                    mask: 0xe0000001,
                    effects: u32::from(bold),
                    height,
                    offset: 0,
                    color,
                    charset: 1,
                    pitch: 0,
                    face: [0; 32],
                };
                for (dest, ch) in value.face.iter_mut().zip(face.encode_utf16()) {
                    *dest = ch;
                }
                value
            };
            SendMessageW(window, WM_SETREDRAW, 0, 0);
            SendMessageW(window, EM_SETBKGNDCOLOR, 0, colors.input as isize);
            let text = text.replace("\r\n", "\r");
            SetWindowTextW(window, wide(&text).as_ptr());
            let body = format("Consolas", colors.text, 210, false);
            SendMessageW(window, EM_SETCHARFORMAT, 4, &body as *const _ as isize); // SCF_ALL
            let mut offset = 0i32;
            for line in text.split('\r') {
                let length = line.encode_utf16().count() as i32;
                let heading = matches!(
                    line,
                    "APPLICATION ACTIVITY"
                        | "REMINDER STATISTICS"
                        | "LAST OBSERVED SESSIONS (UTC)"
                        | "ABOUT THIS SNAPSHOT"
                );
                let muted = line.starts_with('\u{2500}')
                    || line.starts_with("Activity ")
                    || line.starts_with("Reminder ");
                if heading || muted {
                    let range = [offset, offset + length];
                    SendMessageW(window, EM_EXSETSEL, 0, range.as_ptr() as isize);
                    let value = if heading {
                        format("Segoe UI", colors.accent, 230, true)
                    } else {
                        format("Consolas", colors.muted, 210, false)
                    };
                    SendMessageW(window, EM_SETCHARFORMAT, 1, &value as *const _ as isize); // SCF_SELECTION
                }
                offset += length + 1;
            }
            let range = [0i32, 0];
            SendMessageW(window, EM_EXSETSEL, 0, range.as_ptr() as isize);
            SendMessageW(window, WM_SETREDRAW, 1, 0);
            InvalidateRect(window, null(), 0);
        }
    }
}
impl Drop for RichReport {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::FreeLibrary(self.0);
            }
        }
    }
}

impl Drop for Theme {
    fn drop(&mut self) {
        unsafe {
            for object in [
                self.body as HGDIOBJ,
                self.heading as _,
                self.title as _,
                self.mono as _,
                self.background as _,
                self.panel as _,
                self.input as _,
            ] {
                if !object.is_null() {
                    DeleteObject(object);
                }
            }
        }
    }
}

unsafe extern "system" fn combo_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    let colors = current();
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(window, Some(combo_proc), id);
            return DefSubclassProc(window, message, wparam, lparam);
        }
        let result = DefSubclassProc(window, message, wparam, lparam);
        if message == WM_PAINT {
            let mut info: COMBOBOXINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<COMBOBOXINFO>() as u32;
            if GetComboBoxInfo(window, &mut info) != 0 {
                let dc = GetDC(window);
                fill(dc, info.rcButton, colors.input);
                let x = (info.rcButton.left + info.rcButton.right) / 2;
                let y = (info.rcButton.top + info.rcButton.bottom) / 2;
                let pen = CreatePen(PS_SOLID, 2, colors.accent);
                let old = SelectObject(dc, pen as _);
                MoveToEx(dc, x - 4, y - 2, null_mut());
                LineTo(dc, x, y + 2);
                LineTo(dc, x + 4, y - 2);
                SelectObject(dc, old);
                DeleteObject(pen as _);
                let mut r = std::mem::zeroed();
                GetClientRect(window, &mut r);
                let brush = CreateSolidBrush(colors.border);
                FrameRect(dc, &r, brush);
                DeleteObject(brush as _);
                ReleaseDC(window, dc);
            }
        }
        result
    }
}
struct ButtonState {
    checkbox: bool,
    checked: bool,
    hover: bool,
}
unsafe fn button_state(window: HWND) -> Option<*mut ButtonState> {
    // GetWindowSubclass is not exported by unthemed common-controls v5.
    // A private property makes the same subclass state available to WM_DRAWITEM.
    let state = unsafe { GetPropW(window, wide("DatButtonState").as_ptr()) } as *mut ButtonState;
    (!state.is_null()).then_some(state)
}
pub fn button_colors(
    colors: Colors,
    primary: bool,
    hover: bool,
    pressed: bool,
    disabled: bool,
) -> (u32, u32, u32) {
    if disabled {
        return (colors.input, colors.border, colors.muted);
    }
    let fill = if primary {
        if pressed {
            colors.pressed
        } else if hover {
            colors.hover
        } else {
            colors.primary
        }
    } else if pressed {
        colors.border
    } else if hover {
        colors.panel
    } else {
        colors.input
    };
    (
        fill,
        if primary || hover {
            colors.accent
        } else {
            colors.border
        },
        if primary {
            colors.on_primary
        } else {
            colors.text
        },
    )
}
unsafe fn draw_button(theme: &Theme, item: &DRAWITEMSTRUCT) {
    unsafe {
        let Some(state) = button_state(item.hwndItem) else {
            return;
        };
        let state = &*state;
        let colors = theme.colors;
        let disabled = item.itemState & ODS_DISABLED != 0;
        let pressed = item.itemState & ODS_SELECTED != 0;
        let mut r = item.rcItem;
        FillRect(item.hDC, &r, theme.panel);
        let primary = matches!(item.CtlID, 1 | 202);
        let (fill, border, text_color) =
            button_colors(colors, primary, state.hover, pressed, disabled);
        if state.checkbox {
            let y = (r.bottom - 18) / 2;
            rounded(
                item.hDC,
                rect(1, y, 19, y + 18),
                if state.checked { colors.primary } else { fill },
                if state.hover { colors.accent } else { border },
            );
            if state.checked {
                draw_text(
                    item.hDC,
                    "\u{2713}",
                    rect(1, y - 1, 19, y + 19),
                    theme.body,
                    colors.on_primary,
                    DT_CENTER,
                );
            }
            r.left += 29;
        } else {
            rounded(
                item.hDC,
                rect(0, 0, r.right - 1, r.bottom - 1),
                fill,
                border,
            );
        }
        let mut text = [0u16; 256];
        let length = GetWindowTextW(item.hwndItem, text.as_mut_ptr(), 256);
        draw_text(
            item.hDC,
            &String::from_utf16_lossy(&text[..length as usize]),
            r,
            theme.body,
            text_color,
            if state.checkbox { DT_LEFT } else { DT_CENTER },
        );
        if item.itemState & ODS_FOCUS != 0 {
            let brush = CreateSolidBrush(colors.accent);
            FrameRect(
                item.hDC,
                &rect(r.left + 3, r.top + 3, r.right - 4, r.bottom - 4),
                brush,
            );
            DeleteObject(brush as _);
        }
    }
}
unsafe extern "system" fn button_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    };
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(window, Some(button_proc), id);
            RemovePropW(window, wide("DatButtonState").as_ptr());
            drop(Box::from_raw(data as *mut ButtonState));
            return DefSubclassProc(window, message, wparam, lparam);
        }
        let state = &mut *(data as *mut ButtonState);
        match message {
            BM_GETCHECK if state.checkbox => return isize::from(state.checked),
            BM_SETCHECK if state.checkbox => {
                state.checked = wparam != 0;
                InvalidateRect(window, null(), 0);
                return 0;
            }
            // Dialog navigation can send BM_SETSTYLE on focus changes. Never allow
            // it to replace BS_OWNERDRAW with a system-painted push-button style.
            BM_SETSTYLE => {
                return DefSubclassProc(
                    window,
                    message,
                    (wparam & !(BS_TYPEMASK as usize)) | BS_OWNERDRAW as usize,
                    lparam,
                );
            }
            WM_GETDLGCODE => {
                return (DLGC_BUTTON
                    | if state.checkbox {
                        0
                    } else if GetDlgCtrlID(window) == 1 {
                        DLGC_DEFPUSHBUTTON
                    } else {
                        DLGC_UNDEFPUSHBUTTON
                    }) as isize;
            }
            WM_MOUSEMOVE => {
                if !state.hover {
                    state.hover = true;
                    InvalidateRect(window, null(), 0);
                }
                let mut event = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut event);
            }
            WM_MOUSELEAVE => {
                state.hover = false;
                InvalidateRect(window, null(), 0);
            }
            _ => {}
        }
        DefSubclassProc(window, message, wparam, lparam)
    }
}
pub fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}
pub unsafe fn draw_text(dc: HDC, text: &str, mut r: RECT, font: HFONT, color: u32, align: u32) {
    unsafe {
        let previous = SelectObject(dc, font as _);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, color);
        let text = wide(text);
        DrawTextW(
            dc,
            text.as_ptr(),
            (text.len() - 1) as i32,
            &mut r,
            align | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
        SelectObject(dc, previous);
    }
}
unsafe fn font(face: &str, height: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            wide(face).as_ptr(),
        )
    }
}
unsafe fn fill(dc: HDC, r: RECT, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, &r, brush);
        DeleteObject(brush as _);
    }
}
pub(crate) unsafe fn rounded(dc: HDC, r: RECT, color: u32, border: u32) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let pen = CreatePen(PS_SOLID, 1, border);
        let old_brush = SelectObject(dc, brush as _);
        let old_pen = SelectObject(dc, pen as _);
        RoundRect(dc, r.left, r.top, r.right, r.bottom, 10, 10);
        SelectObject(dc, old_brush);
        SelectObject(dc, old_pen);
        DeleteObject(brush as _);
        DeleteObject(pen as _);
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn table_markup_escapes_data_and_preserves_unicode() {
        assert_eq!(rtf_escape("{x}\\"), "\\{x\\}\\\\");
        assert!(rtf_escape("\u{1f4a7}").contains("\\u-"));
        let output = table_rtf(
            &["Activity", "Sessions"],
            &[vec!["App {test}".into(), "3".into()]],
            "Local only",
            &[200, 90],
        );
        assert!(output.contains("\\cellx3000"));
        assert!(output.contains("\\clcbpat4"));
        assert!(output.contains("\\clcbpat3"));
        assert!(output.contains("App \\{test\\}"));
    }
    #[test]
    fn palette_button_states_remain_distinct_and_readable() {
        for palette in crate::settings::ColorPalette::ALL {
            let colors = crate::ui_palette::colors_for(palette);
            let normal = button_colors(colors, true, false, false, false);
            assert_ne!(normal, button_colors(colors, true, true, false, false));
            assert_ne!(normal, button_colors(colors, true, false, true, false));
            assert_eq!(
                button_colors(colors, true, false, false, true).2,
                colors.muted
            );
            assert_ne!(normal.0, normal.2);
        }
    }
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn native_themed_windows() {
        let settings =
            std::sync::Arc::new(std::sync::Mutex::new(crate::settings::Settings::default()));
        crate::settings_window::show(settings.clone());
        let window = crate::tests::find_own_window("DatSettings");
        assert!(!window.is_null());
        crate::settings_window::assert_native_help_layout(window);
        unsafe {
            let mut client = std::mem::zeroed();
            GetClientRect(window, &mut client);
            let mut positions = Vec::new();
            for id in [2, 110, 1] {
                let mut r = std::mem::zeroed();
                GetWindowRect(GetDlgItem(window, id), &mut r);
                MapWindowPoints(null_mut(), window, &mut r as *mut _ as _, 2);
                positions.push(r);
            }
            assert!(
                positions[0].right < positions[1].left && positions[1].right < positions[2].left
            );
            assert!((positions[0].left + positions[2].right - client.right).abs() <= 1);
        }
        pump();
        capture(window, "settings");
        unsafe {
            assert!(
                GetDlgItem(window, 304).is_null(),
                "Removed Settings footer text should not exist"
            );
        }
        unsafe {
            assert_eq!(SendDlgItemMessageW(window, 109, CB_GETCOUNT, 0, 0), 4);
            assert_eq!(SendDlgItemMessageW(window, 111, CB_GETCOUNT, 0, 0), 2);
            assert_eq!(SendDlgItemMessageW(window, 111, CB_GETCURSEL, 0, 0), 0);
        }
        for (palette, name) in [
            (crate::settings::ColorPalette::DatDefault, "dat"),
            (crate::settings::ColorPalette::Dark, "dark"),
            (crate::settings::ColorPalette::Light, "light"),
            (crate::settings::ColorPalette::System, "system"),
        ] {
            unsafe {
                SendDlgItemMessageW(window, 109, CB_SETCURSEL, palette as usize, 0);
            }
            crate::ui_palette::apply(palette);
            pump();
            capture(window, &format!("settings-{name}"));
            unsafe {
                for id in [1, 2, 107, 110] {
                    let button = GetDlgItem(window, id);
                    assert!(!button.is_null());
                    SendMessageW(button, BM_SETSTYLE, BS_DEFPUSHBUTTON as usize, 1);
                    assert_eq!(
                        GetWindowLongPtrW(button, GWL_STYLE) as u32 & BS_TYPEMASK as u32,
                        BS_OWNERDRAW as u32
                    );
                    SendMessageW(button, WM_MOUSEMOVE, 0, ((8isize) << 16) | 8);
                    SendMessageW(button, BM_SETSTATE, 1, 0);
                    UpdateWindow(button);
                    assert_ne!(
                        SendMessageW(button, BM_GETSTATE, 0, 0) & BST_PUSHED as isize,
                        0
                    );
                }
            }
            pump();
            capture(window, &format!("settings-{name}-pressed"));
            unsafe {
                let save = GetDlgItem(window, 1);
                windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(save);
                assert_eq!(
                    SendMessageW(save, WM_GETDLGCODE, 0, 0) as u32 & DLGC_DEFPUSHBUTTON,
                    DLGC_DEFPUSHBUTTON
                );
                windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    GetDlgItem(window, 110),
                    0,
                );
            }
            pump();
            capture(window, &format!("settings-{name}-focus-disabled"));
            unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    GetDlgItem(window, 110),
                    1,
                );
            }

            unsafe {
                for id in [1, 2, 107, 110] {
                    let button = GetDlgItem(window, id);
                    SendMessageW(button, BM_SETSTATE, 0, 0);
                    SendMessageW(button, WM_MOUSELEAVE, 0, 0);
                }
            }
        }
        crate::ui_palette::apply(crate::settings::ColorPalette::DatDefault);
        pump();
        unsafe {
            // Native button input and checkbox state remain in the original control.
            let checkbox = GetDlgItem(window, 108);
            SendMessageW(checkbox, BM_CLICK, 0, 0);
            assert_eq!(SendMessageW(checkbox, BM_GETCHECK, 0, 0), 1);
            SendMessageW(checkbox, BM_CLICK, 0, 0);
            assert_eq!(SendMessageW(checkbox, BM_GETCHECK, 0, 0), 0);
            SendMessageW(window, WM_COMMAND, 2, 0);
            assert_eq!(IsWindow(window), 0);
        }
        assert_eq!(
            *settings.lock().unwrap(),
            crate::settings::Settings::default()
        );
        crate::statistics_window::show_for_path(None);
        let window = crate::tests::find_own_window("DatStatistics");
        assert!(!window.is_null());
        crate::statistics_window::preview_data(window);
        pump();
        capture(window, "statistics");
        for (palette, name) in [
            (crate::settings::ColorPalette::DatDefault, "dat"),
            (crate::settings::ColorPalette::Dark, "dark"),
            (crate::settings::ColorPalette::Light, "light"),
            (crate::settings::ColorPalette::System, "system"),
        ] {
            crate::ui_palette::apply(palette);
            pump();
            crate::statistics_window::preview_data(window);
            pump();
            capture(window, &format!("statistics-{name}"));
        }
        crate::ui_palette::apply(crate::settings::ColorPalette::DatDefault);
        pump();
        crate::statistics_window::preview_data(window);
        unsafe {
            SetWindowPos(
                window,
                null_mut(),
                0,
                0,
                1100,
                680,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        pump();
        capture(window, "statistics-small");
        unsafe {
            for (id, expected) in [
                (201, "World of Warcraft"),
                (203, "Drink water"),
                (204, "2026-10-11"),
            ] {
                let mut text = [0u16; 8192];
                let count =
                    GetWindowTextW(GetDlgItem(window, id), text.as_mut_ptr(), text.len() as i32);
                let text = String::from_utf16_lossy(&text[..count as usize]);
                assert!(
                    text.contains(expected),
                    "Missing table data in {id}: {text}"
                );
                assert!(!text.contains("{\\rtf1"));
            }
            let mut client = std::mem::zeroed();
            GetClientRect(window, &mut client);
            for id in [201, 203, 204, 202, 212, 215, 2, 206, 207, 208] {
                let mut child = std::mem::zeroed();
                GetWindowRect(GetDlgItem(window, id), &mut child);
                MapWindowPoints(null_mut(), window, &mut child as *mut _ as _, 2);
                assert!(
                    child.left >= 0
                        && child.top >= 0
                        && child.right <= client.right
                        && child.bottom <= client.bottom
                );
            }
            assert_eq!(SendDlgItemMessageW(window, 215, CB_GETCOUNT, 0, 0), 4);
            assert_eq!(SendDlgItemMessageW(window, 215, CB_GETCURSEL, 0, 0), 3);
            let mut footer = Vec::new();
            for id in [202, 212, 2] {
                let mut bounds: RECT = std::mem::zeroed();
                let button = GetDlgItem(window, id);
                GetWindowRect(button, &mut bounds);
                MapWindowPoints(null_mut(), window, &mut bounds as *mut _ as _, 2);
                footer.push(bounds);
                assert_eq!(
                    GetWindowLongPtrW(button, GWL_STYLE) as u32 & BS_TYPEMASK as u32,
                    BS_OWNERDRAW as u32
                );
            }
            assert_eq!(footer[0].left, client.right - footer[2].right);
            assert!(footer[0].right < footer[1].left && footer[1].right < footer[2].left);
            assert_eq!(footer[0].top, footer[1].top);
            let report_text = |id| {
                let mut text = [0u16; 8192];
                let count =
                    GetWindowTextW(GetDlgItem(window, id), text.as_mut_ptr(), text.len() as i32);
                String::from_utf16_lossy(&text[..count as usize])
            };
            assert!(GetDlgItem(window, 205).is_null(), "Search was removed");
            for (button, report, name) in [
                (206, 201, "applications"),
                (207, 203, "reminders"),
                (208, 204, "sessions"),
            ] {
                SendDlgItemMessageW(window, button, BM_CLICK, 0, 0);
                pump();
                assert_ne!(IsWindowVisible(GetDlgItem(window, 209)), 0);
                for id in [201, 203, 204] {
                    assert_eq!(IsWindowVisible(GetDlgItem(window, id)) != 0, id == report);
                    assert_eq!(
                        GetWindowLongPtrW(GetDlgItem(window, id), GWL_STYLE) as u32
                            & (WS_VSCROLL | WS_HSCROLL),
                        0
                    );
                }
                assert!(!report_text(report).is_empty());
                capture(window, &format!("statistics-detail-{name}"));
                SendDlgItemMessageW(window, 209, BM_CLICK, 0, 0);
                assert_eq!(IsWindowVisible(GetDlgItem(window, 209)), 0);
            }
            crate::statistics_window::preview_many_rows(window);
            SendDlgItemMessageW(window, 206, BM_CLICK, 0, 0);
            let mut all_pages = String::new();
            for _ in 0..25 {
                let page = report_text(201);
                all_pages.push_str(&page);
                if windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(GetDlgItem(
                    window, 211,
                )) == 0
                {
                    break;
                }
                SendDlgItemMessageW(window, 211, BM_CLICK, 0, 0);
                assert_ne!(report_text(201), page);
            }
            for index in 0..25 {
                assert!(all_pages.contains(&format!("Application {index:02}")));
            }
            assert_ne!(
                windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(GetDlgItem(
                    window, 210
                )),
                0
            );
            SendDlgItemMessageW(window, 210, BM_CLICK, 0, 0);
            pump();
            capture(window, "statistics-paginated");
            crate::statistics_window::show_for_path(None);
            assert_eq!(
                IsWindowVisible(GetDlgItem(window, 209)),
                0,
                "Opening Statistics returns to overview"
            );
            SendMessageW(window, WM_COMMAND, 202, 0);
            SendMessageW(window, WM_COMMAND, 2, 0);
            assert_eq!(IsWindow(window), 0);
        }
    }
    fn pump() {
        for _ in 0..15 {
            unsafe {
                let mut message = std::mem::zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    // Opt-in captures contain only synthetic data and our own window's client area.
    pub(crate) fn capture(window: HWND, name: &str) {
        if std::env::var_os("DAT_UI_CAPTURE").is_none() {
            return;
        }
        unsafe {
            let mut r = std::mem::zeroed();
            GetClientRect(window, &mut r);
            let source = GetDC(window);
            let dc = CreateCompatibleDC(source);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: r.right,
                    biHeight: -r.bottom,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                },
                ..std::mem::zeroed()
            };
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            assert!(!bitmap.is_null());
            let previous = SelectObject(dc, bitmap as _);
            assert_ne!(
                BitBlt(dc, 0, 0, r.right, r.bottom, source, 0, 0, SRCCOPY),
                0
            );
            GdiFlush();
            let len = (r.right * r.bottom * 4) as usize;
            let mut bytes = Vec::new();
            bytes.extend(b"BM");
            bytes.extend(((54 + len) as u32).to_le_bytes());
            bytes.extend([0u8; 4]);
            bytes.extend(54u32.to_le_bytes());
            bytes.extend(40u32.to_le_bytes());
            bytes.extend(r.right.to_le_bytes());
            bytes.extend((-r.bottom).to_le_bytes());
            bytes.extend(1u16.to_le_bytes());
            bytes.extend(32u16.to_le_bytes());
            bytes.extend([0u8; 24]);
            bytes.extend(std::slice::from_raw_parts(bits as *const u8, len));
            std::fs::create_dir_all("target/ui-previews").unwrap();
            std::fs::write(format!("target/ui-previews/{name}.bmp"), bytes).unwrap();
            SelectObject(dc, previous);
            DeleteObject(bitmap as _);
            DeleteDC(dc);
            ReleaseDC(window, source);
        }
    }
}
