use std::{
    ptr::{null, null_mut},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicIsize, Ordering},
        mpsc::Sender,
    },
    thread,
};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject, EndPaint, FillRect,
        FrameRect, GetStockObject, HDC, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode,
        SetTextColor, TRANSPARENT, TextOutW,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW,
        GetClientRect, GetMessageW, GetSystemMetrics, GetWindowRect, HWND_TOPMOST, IDC_ARROW,
        KillTimer, LWA_COLORKEY, LoadCursorW, MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage,
        RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow,
        TranslateMessage, WM_APP, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONUP,
        WM_MOUSEACTIVATE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    },
};

use crate::reminders::{ReminderAction, ReminderSnapshot, ReminderToast};

const HUD_UPDATE: u32 = WM_APP + 20;

const WIDTH: i32 = 300;
const ACTIVE_HEIGHT: i32 = 92;
const PREVIEW_HEIGHT: i32 = 38;
const OVERFLOW_HEIGHT: i32 = 24;

const GAP: i32 = 4;
const OUTER_MARGIN: i32 = 10;

const MAX_PREVIEWS: usize = 2;

const TRANSPARENT_KEY: u32 = rgb(1, 2, 3);

const CARD_BACKGROUND: u32 = rgb(20, 20, 24);
const CARD_BORDER: u32 = rgb(150, 150, 165);

const TEXT_PRIMARY: u32 = rgb(245, 245, 248);
const TEXT_SECONDARY: u32 = rgb(205, 205, 214);
const ICON_COLOR: u32 = rgb(120, 165, 255);

static ACTION_SENDER: OnceLock<Sender<ReminderAction>> = OnceLock::new();

static SNAPSHOT: OnceLock<Mutex<ReminderSnapshot>> = OnceLock::new();

static HUD_WINDOW: AtomicIsize = AtomicIsize::new(0);

pub fn start(action_sender: Sender<ReminderAction>) {
    if ACTION_SENDER.set(action_sender).is_err() {
        eprintln!("Reminder HUD action sender was already initialized.");
        return;
    }

    let _ = SNAPSHOT.set(Mutex::new(ReminderSnapshot::default()));

    thread::spawn(|| {
        if let Err(error) = run_hud() {
            eprintln!("Reminder HUD error: {error}");
        }
    });
}

pub fn render(snapshot: ReminderSnapshot) {
    if let Some(state) = SNAPSHOT.get()
        && let Ok(mut current) = state.lock()
    {
        *current = snapshot;
    }

    let window = HUD_WINDOW.load(Ordering::SeqCst) as HWND;

    if !window.is_null() {
        unsafe {
            PostMessageW(window, HUD_UPDATE, 0, 0);
        }
    }
}

pub fn shutdown() {
    let window = HUD_WINDOW.load(Ordering::SeqCst) as HWND;

    if !window.is_null() {
        unsafe {
            PostMessageW(window, WM_CLOSE, 0, 0);
        }
    }
}

fn run_hud() -> Result<(), &'static str> {
    unsafe {
        let instance = GetModuleHandleW(null());

        if instance.is_null() {
            return Err("GetModuleHandleW failed");
        }

        let class_name = wide("DiscordActivityTrackerReminderHud");

        let mut window_class: WNDCLASSW = std::mem::zeroed();

        window_class.lpfnWndProc = Some(window_proc);
        window_class.hInstance = instance;
        window_class.lpszClassName = class_name.as_ptr();
        window_class.hCursor = LoadCursorW(null_mut(), IDC_ARROW);

        if RegisterClassW(&window_class) == 0 {
            return Err("RegisterClassW failed");
        }

        let window = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );

        if window.is_null() {
            return Err("CreateWindowExW failed");
        }

        if SetLayeredWindowAttributes(window, TRANSPARENT_KEY, 0, LWA_COLORKEY) == 0 {
            DestroyWindow(window);
            return Err("SetLayeredWindowAttributes failed");
        }

        HUD_WINDOW.store(window as isize, Ordering::SeqCst);

        apply_snapshot(window);

        let mut message: MSG = std::mem::zeroed();

        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        HUD_WINDOW.store(0, Ordering::SeqCst);

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
        HUD_UPDATE => {
            apply_snapshot(window);
            0
        }

        WM_TIMER => {
            if !current_snapshot().pending.is_empty() {
                unsafe {
                    // Reassert z-order without activation. Losing focus is never
                    // a reminder action and must not demote a pending HUD.
                    SetWindowPos(
                        window,
                        HWND_TOPMOST,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    InvalidateRect(window, null(), 0);
                }
            }
            0
        }

        WM_PAINT => {
            paint(window);
            0
        }

        WM_LBUTTONUP => {
            handle_click(lparam);
            0
        }

        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,

        WM_ERASEBKGND => 1,

        WM_CLOSE => {
            unsafe {
                DestroyWindow(window);
            }

            0
        }

        WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }

            0
        }

        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn apply_snapshot(window: HWND) {
    let snapshot = current_snapshot();

    if snapshot.pending.is_empty() {
        unsafe {
            KillTimer(window, 1);
            ShowWindow(window, SW_HIDE);
        }

        return;
    }

    let layout = Layout::new(snapshot.pending.len());

    let (x, y) = taskbar_anchor(layout.width, layout.height);

    unsafe {
        SetTimer(window, 1, 1000, None);
        SetWindowPos(
            window,
            HWND_TOPMOST,
            x,
            y,
            layout.width,
            layout.height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );

        InvalidateRect(window, null(), 0);
    }
}

fn paint(window: HWND) {
    let snapshot = current_snapshot();

    unsafe {
        let mut paint: PAINTSTRUCT = std::mem::zeroed();

        let hdc = BeginPaint(window, &mut paint);

        let mut client: RECT = std::mem::zeroed();

        GetClientRect(window, &mut client);

        let background = UiRect {
            left: client.left,
            top: client.top,
            right: client.right,
            bottom: client.bottom,
        };

        fill_rect(hdc, background, TRANSPARENT_KEY);

        SetBkMode(hdc, TRANSPARENT as i32);

        let font = GetStockObject(DEFAULT_GUI_FONT);
        let previous_font = SelectObject(hdc, font);

        if !snapshot.pending.is_empty() {
            let layout = Layout::new(snapshot.pending.len());

            for (pending_index, rectangle) in &layout.previews {
                if let Some(toast) = snapshot.pending.get(*pending_index) {
                    paint_toast(hdc, *rectangle, toast, true);
                }
            }

            if let Some(active) = snapshot.pending.first() {
                paint_toast(hdc, layout.active, active, false);

                SetTextColor(hdc, TEXT_PRIMARY);

                text_out(hdc, layout.done.left + 4, layout.done.top + 5, "Done");

                text_out(hdc, layout.snooze.left + 4, layout.snooze.top + 5, "Snooze");
            }

            if let Some(overflow_rectangle) = layout.overflow {
                fill_rect(hdc, overflow_rectangle, CARD_BACKGROUND);

                frame_rect(hdc, overflow_rectangle, CARD_BORDER);

                SetTextColor(hdc, TEXT_PRIMARY);

                text_out(
                    hdc,
                    overflow_rectangle.left + 14,
                    overflow_rectangle.top + 4,
                    &format!("+{}", layout.hidden_count),
                );
            }
        }

        if !previous_font.is_null() {
            SelectObject(hdc, previous_font);
        }

        EndPaint(window, &paint);
    }
}

fn paint_toast(hdc: HDC, rectangle: UiRect, toast: &ReminderToast, compact: bool) {
    fill_rect(hdc, rectangle, CARD_BACKGROUND);
    frame_rect(hdc, rectangle, CARD_BORDER);

    let icon = icon_glyph(toast.icon.as_deref());

    let vertical_offset = if compact { 11 } else { 16 };

    unsafe {
        SetTextColor(hdc, ICON_COLOR);
    }

    text_out(
        hdc,
        rectangle.left + 10,
        rectangle.top + vertical_offset,
        icon,
    );

    unsafe {
        SetTextColor(hdc, TEXT_PRIMARY);
    }

    let maximum_chars = if compact { 30 } else { 34 };
    let title = truncate_text(&toast.name, maximum_chars);

    text_out(
        hdc,
        rectangle.left + 36,
        rectangle.top + vertical_offset,
        &title,
    );

    if !compact {
        unsafe {
            SetTextColor(hdc, TEXT_SECONDARY);
        }
        text_out(
            hdc,
            rectangle.left + 36,
            rectangle.top + 36,
            &crate::reminders::format_elapsed(toast.first_due.elapsed().as_secs()),
        );
    }
}

fn handle_click(lparam: LPARAM) {
    let snapshot = current_snapshot();

    let Some(active) = snapshot.pending.first() else {
        return;
    };

    let layout = Layout::new(snapshot.pending.len());

    let (x, y) = point_from_lparam(lparam);

    if layout.done.contains(x, y) {
        send_action(ReminderAction::Done(active.id.clone()));
        return;
    }

    if layout.snooze.contains(x, y) {
        send_action(ReminderAction::Snooze(active.id.clone()));
        return;
    }

    for (pending_index, rectangle) in &layout.previews {
        if rectangle.contains(x, y)
            && let Some(toast) = snapshot.pending.get(*pending_index)
        {
            send_action(ReminderAction::Activate(toast.id.clone()));
            return;
        }
    }
}

fn send_action(action: ReminderAction) {
    if let Some(sender) = ACTION_SENDER.get() {
        let _ = sender.send(action);
    }
}

fn current_snapshot() -> ReminderSnapshot {
    SNAPSHOT
        .get()
        .and_then(|snapshot| snapshot.lock().ok().map(|snapshot| snapshot.clone()))
        .unwrap_or_default()
}

fn taskbar_anchor(width: i32, height: i32) -> (i32, i32) {
    let screen_width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let screen_height = unsafe { GetSystemMetrics(SM_CYSCREEN) };

    let taskbar_class = wide("Shell_TrayWnd");

    let taskbar = unsafe { FindWindowW(taskbar_class.as_ptr(), null()) };

    if !taskbar.is_null() {
        let mut rectangle: RECT = unsafe { std::mem::zeroed() };

        if unsafe { GetWindowRect(taskbar, &mut rectangle) } != 0 {
            let taskbar_width = rectangle.right - rectangle.left;
            let taskbar_height = rectangle.bottom - rectangle.top;

            if taskbar_width >= taskbar_height {
                // Horizontal taskbar.
                if rectangle.top > screen_height / 2 {
                    // Bottom taskbar.
                    return (
                        rectangle.right - width - OUTER_MARGIN,
                        rectangle.top - height - OUTER_MARGIN,
                    );
                }

                // Top taskbar.
                return (
                    rectangle.right - width - OUTER_MARGIN,
                    rectangle.bottom + OUTER_MARGIN,
                );
            }

            // Vertical taskbar.
            if rectangle.left > screen_width / 2 {
                // Right.
                return (
                    rectangle.left - width - OUTER_MARGIN,
                    rectangle.bottom - height - OUTER_MARGIN,
                );
            }

            // Left.
            return (
                rectangle.right + OUTER_MARGIN,
                rectangle.bottom - height - OUTER_MARGIN,
            );
        }
    }

    // Safe fallback when Shell_TrayWnd cannot be found.
    (
        screen_width - width - OUTER_MARGIN,
        screen_height - height - 64,
    )
}

#[derive(Debug, Clone, Copy)]
struct UiRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl UiRect {
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    fn as_windows_rect(self) -> RECT {
        RECT {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }
}

struct Layout {
    width: i32,
    height: i32,

    active: UiRect,

    previews: Vec<(usize, UiRect)>,

    overflow: Option<UiRect>,
    hidden_count: usize,

    done: UiRect,
    snooze: UiRect,
}

impl Layout {
    fn new(pending_count: usize) -> Self {
        let preview_count = pending_count.saturating_sub(1).min(MAX_PREVIEWS);

        let hidden_count = pending_count.saturating_sub(1 + preview_count);

        let overflow_space = if hidden_count > 0 {
            OVERFLOW_HEIGHT + GAP
        } else {
            0
        };

        let height =
            ACTIVE_HEIGHT + (preview_count as i32 * (PREVIEW_HEIGHT + GAP)) + overflow_space;

        let active = UiRect {
            left: 0,
            top: height - ACTIVE_HEIGHT,
            right: WIDTH,
            bottom: height,
        };

        let mut previews = Vec::with_capacity(preview_count);

        let mut cursor = active.top - GAP;

        for index in 0..preview_count {
            cursor -= PREVIEW_HEIGHT;

            // Each card above becomes slightly narrower,
            // reproducing the stacked appearance in the mockup.
            let inset = ((index + 1) as i32) * 3;

            previews.push((
                index + 1,
                UiRect {
                    left: inset,
                    top: cursor,
                    right: WIDTH - inset,
                    bottom: cursor + PREVIEW_HEIGHT,
                },
            ));

            cursor -= GAP;
        }

        let overflow = if hidden_count > 0 {
            let badge_width = 58;

            Some(UiRect {
                left: (WIDTH - badge_width) / 2,
                top: 0,
                right: (WIDTH + badge_width) / 2,
                bottom: OVERFLOW_HEIGHT,
            })
        } else {
            None
        };

        let button_top = active.bottom - 34;

        let done = UiRect {
            left: active.left + 8,
            top: button_top,
            right: active.left + 70,
            bottom: active.bottom - 6,
        };

        let snooze = UiRect {
            left: active.left + 82,
            top: button_top,
            right: active.left + 160,
            bottom: active.bottom - 6,
        };

        Self {
            width: WIDTH,
            height,

            active,

            previews,

            overflow,
            hidden_count,

            done,
            snooze,
        }
    }
}

fn fill_rect(hdc: HDC, rectangle: UiRect, color: u32) {
    let brush = unsafe { CreateSolidBrush(color) };

    let windows_rectangle = rectangle.as_windows_rect();

    unsafe {
        FillRect(hdc, &windows_rectangle, brush);
        DeleteObject(brush as _);
    }
}

fn frame_rect(hdc: HDC, rectangle: UiRect, color: u32) {
    let brush = unsafe { CreateSolidBrush(color) };

    let windows_rectangle = rectangle.as_windows_rect();

    unsafe {
        FrameRect(hdc, &windows_rectangle, brush);
        DeleteObject(brush as _);
    }
}

fn text_out(hdc: HDC, x: i32, y: i32, text: &str) {
    let text: Vec<u16> = text.encode_utf16().collect();

    unsafe {
        TextOutW(hdc, x, y, text.as_ptr(), text.len() as i32);
    }
}

fn icon_glyph(icon: Option<&str>) -> &str {
    match icon.unwrap_or_default() {
        "water" => "💧",
        "stand" => "↑",
        "move" => "↑",
        "eyes" => "◉",
        "" => "•",
        other => other,
    }
}

fn truncate_text(text: &str, maximum: usize) -> String {
    if text.chars().count() <= maximum {
        return text.to_owned();
    }

    let mut result: String = text.chars().take(maximum.saturating_sub(1)).collect();

    result.push('…');

    result
}

fn point_from_lparam(lparam: LPARAM) -> (i32, i32) {
    let value = lparam as u32;

    let x = (value & 0xffff) as u16 as i16 as i32;
    let y = ((value >> 16) & 0xffff) as u16 as i16 as i32;

    (x, y)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

const fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    red as u32 | ((green as u32) << 8) | ((blue as u32) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stacked_layout_preserves_active_buttons_previews_and_overflow() {
        let single = Layout::new(1);
        assert!(single.previews.is_empty());
        assert!(single.overflow.is_none());
        let many = Layout::new(5);
        assert_eq!(many.previews.len(), 2);
        assert_eq!(many.hidden_count, 2);
        assert!(many.overflow.is_some());
        assert!(many.done.top > many.active.top + 50);
        assert!(many.snooze.top == many.done.top);
        assert!(
            many.previews
                .iter()
                .all(|(_, r)| r.bottom < many.active.top)
        );
        assert_eq!(many.previews[0].0, 1);
        assert_eq!(many.previews[1].0, 2);
    }
}
