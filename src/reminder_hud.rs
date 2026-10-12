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
        SetTextColor, TRANSPARENT,
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

use windows_sys::Win32::{
    Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DT_RIGHT, DeleteDC, IntersectClipRect,
        SRCCOPY,
    },
    UI::WindowsAndMessaging::{DI_NORMAL, DrawIconEx, GWL_EXSTYLE, GetWindowLongPtrW},
};

use crate::reminders::{ReminderAction, ReminderSnapshot, ReminderToast};

const HUD_UPDATE: u32 = WM_APP + 20;

const WIDTH: i32 = 300;
const ACTIVE_HEIGHT: i32 = 120;
const FOOTER_HEIGHT: i32 = 28;
const PREVIEW_HEIGHT: i32 = 38;
const OVERFLOW_HEIGHT: i32 = 24;

const GAP: i32 = 4;
const OUTER_MARGIN: i32 = 10;

const MAX_PREVIEWS: usize = 2;

const TRANSPARENT_KEY: u32 = rgb(1, 2, 3);

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
        if *current == snapshot {
            return;
        }
        *current = snapshot;
    }

    let window = HUD_WINDOW.load(Ordering::SeqCst) as HWND;

    if !window.is_null() {
        unsafe {
            PostMessageW(window, HUD_UPDATE, 0, 0);
        }
    }
}

thread_local! {
static REMINDER_ICONS: [Option<crate::tray_icon::TrayIcon>; 5] = std::array::from_fn(|i| unsafe {
    use windows_sys::Win32::UI::WindowsAndMessaging::{LoadImageW, IMAGE_ICON};
    let icon = LoadImageW(GetModuleHandleW(null()), (11+i) as *const u16, IMAGE_ICON, 32, 32, 0);
    if icon.is_null() { eprintln!("Could not load embedded reminder icon {}",11+i); None }
    else { Some(crate::tray_icon::TrayIcon(icon)) }
});
static BRAND: Option<crate::tray_icon::TrayIcon> = crate::tray_icon::TrayIcon::load(16).ok();
static LAST_SECONDS: std::cell::Cell<Option<u64>> = const {std::cell::Cell::new(None)};
static POINTER: std::cell::Cell<(i32,i32,bool)> = const {std::cell::Cell::new((-1,-1,false))};}
pub fn palette_changed() {
    let window = HUD_WINDOW.load(Ordering::SeqCst) as HWND;
    if !window.is_null() {
        unsafe {
            PostMessageW(window, crate::ui_palette::CHANGED, 0, 0);
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

// Fonts are private to this process and remain registered for the HUD thread lifetime.
struct HudFonts(Vec<isize>);
impl HudFonts {
    fn load() -> Self {
        let mut handles = Vec::new();
        for bytes in [
            include_bytes!("../assets/fonts/Rajdhani-SemiBold.ttf").as_slice(),
            include_bytes!("../assets/fonts/Oxanium-Regular.ttf").as_slice(),
            include_bytes!("../assets/fonts/Oxanium-Medium.ttf").as_slice(),
            include_bytes!("../assets/fonts/Oxanium-SemiBold.ttf").as_slice(),
            include_bytes!("../assets/fonts/Oxanium-Bold.ttf").as_slice(),
        ] {
            let mut count = 0;
            let handle = unsafe {
                windows_sys::Win32::Graphics::Gdi::AddFontMemResourceEx(
                    bytes.as_ptr() as _,
                    bytes.len() as u32,
                    null(),
                    &raw mut count,
                )
            };
            if handle.is_null() {
                eprintln!(
                    "Could not register bundled reminder font; Windows fallback will be used"
                );
            } else {
                handles.push(handle as isize);
            }
        }
        Self(handles)
    }
}
impl Drop for HudFonts {
    fn drop(&mut self) {
        for &handle in &self.0 {
            unsafe {
                windows_sys::Win32::Graphics::Gdi::RemoveFontMemResourceEx(handle as _);
            }
        }
    }
}

fn run_hud() -> Result<(), &'static str> {
    let _fonts = HudFonts::load();
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
    use windows_sys::Win32::UI::{
        Controls::WM_MOUSELEAVE,
        Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
        WindowsAndMessaging::{WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_SETTINGCHANGE},
    };
    match message {
        crate::ui_palette::CHANGED => {
            unsafe {
                InvalidateRect(window, null(), 0);
            }
            0
        }
        WM_SETTINGCHANGE => {
            crate::ui_palette::system_changed();
            0
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN => {
            let (x, y) = point_from_lparam(lparam);
            update_pointer(window, (x, y, message == WM_LBUTTONDOWN || wparam & 1 != 0));
            unsafe {
                let mut event = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut event);
            }
            0
        }
        WM_MOUSELEAVE => {
            update_pointer(window, (-1, -1, false));
            0
        }
        HUD_UPDATE => {
            apply_snapshot(window);
            0
        }

        WM_TIMER => {
            let snapshot = current_snapshot();
            if let Some(active) = snapshot.pending.first() {
                if wparam == 1 {
                    let seconds = active.first_due.elapsed().as_secs();
                    if LAST_SECONDS.with(|last| last.replace(Some(seconds))) != Some(seconds) {
                        invalidate(window, Layout::for_snapshot(&snapshot).timer_rect);
                    }
                } else if wparam == 2 {
                    // Z-order repair is independent of rendering; never re-show a toast.
                    unsafe {
                        if GetWindowLongPtrW(window, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST == 0 {
                            SetWindowPos(
                                window,
                                HWND_TOPMOST,
                                0,
                                0,
                                0,
                                0,
                                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                            );
                        }
                    }
                }
            }
            0
        }

        WM_PAINT => {
            paint(window);
            0
        }

        WM_LBUTTONUP => {
            let (x, y) = point_from_lparam(lparam);
            update_pointer(window, (x, y, false));
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
            KillTimer(window, 2);
            ShowWindow(window, SW_HIDE);
        }

        return;
    }

    let layout = Layout::for_snapshot(&snapshot);

    let (x, y) = taskbar_anchor(layout.width, layout.height);

    unsafe {
        SetTimer(window, 1, 1000, None);
        SetTimer(window, 2, 1000, None);
        LAST_SECONDS.with(|last| last.set(None));
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

        let visible = BeginPaint(window, &mut paint);
        if paint.rcPaint.right <= paint.rcPaint.left || paint.rcPaint.bottom <= paint.rcPaint.top {
            EndPaint(window, &paint);
            return;
        }

        let mut client: RECT = std::mem::zeroed();

        GetClientRect(window, &mut client);

        // Only the dirty rectangle is transferred to the visible surface.
        let hdc = CreateCompatibleDC(visible);
        let bitmap = CreateCompatibleBitmap(visible, client.right.max(1), client.bottom.max(1));
        if hdc.is_null() || bitmap.is_null() {
            eprintln!("Could not allocate reminder paint buffer");
            if !hdc.is_null() {
                DeleteDC(hdc);
            }
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            EndPaint(window, &paint);
            return;
        }
        let old_bitmap = SelectObject(hdc, bitmap);
        let dirty = UiRect::from_windows(paint.rcPaint);

        IntersectClipRect(hdc, dirty.left, dirty.top, dirty.right, dirty.bottom);
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
            let layout = Layout::for_snapshot(&snapshot);

            for (pending_index, rectangle) in &layout.previews {
                if rectangle.intersects(dirty)
                    && let Some(toast) = snapshot.pending.get(*pending_index)
                {
                    paint_toast(hdc, *rectangle, toast, true, *rectangle);
                }
            }

            if layout.active.intersects(dirty)
                && let Some(active) = snapshot.pending.first()
            {
                // Dynamic invalidations avoid drawing static title, border and footer.
                let dynamic = dirty.inside(layout.timer_rect)
                    || dirty.inside(layout.done)
                    || dirty.inside(layout.snooze);
                if dynamic {
                    fill_rect(hdc, dirty, crate::ui_palette::current().panel);
                } else {
                    paint_toast(hdc, layout.active, active, false, layout.title_rect);
                    paint_footer(hdc, layout.footer, active.interval_minutes);
                }
                if dirty.intersects(layout.timer_rect) {
                    styled_text(
                        hdc,
                        layout.timer_rect,
                        &crate::reminders::format_elapsed(active.first_due.elapsed().as_secs()),
                        if active.first_due.elapsed().as_secs() >= 3600 {
                            14
                        } else {
                            20
                        },
                        700,
                        crate::ui_palette::current().accent,
                        windows_sys::Win32::Graphics::Gdi::DT_CENTER,
                    );
                }

                for (rectangle, icon, primary) in [
                    (layout.done, "\u{2713}", true),
                    (layout.snooze, "\u{25f7}", false),
                ] {
                    if !rectangle.intersects(dirty) {
                        continue;
                    }
                    let (x, y, down) = POINTER.with(|p| p.get());
                    let hover = rectangle.contains(x, y);
                    let (mut fill, mut border, mut text) = crate::ui_theme::button_colors(
                        crate::ui_palette::current(),
                        primary,
                        hover,
                        hover && down,
                        false,
                    );
                    let colors = crate::ui_palette::current();
                    if primary {
                        fill = blend(
                            colors.panel,
                            fill,
                            if hover && down {
                                45
                            } else if hover {
                                85
                            } else {
                                65
                            },
                        );
                        border = colors.accent;
                        text = colors.accent;
                    }
                    layered_panel(hdc, rectangle, fill, border);
                    draw_text(
                        hdc,
                        rectangle.inset(3),
                        icon,
                        ("Segoe UI Symbol", 18, 400),
                        text,
                        windows_sys::Win32::Graphics::Gdi::DT_CENTER,
                    );
                }
            }

            if let Some(overflow_rectangle) = layout.overflow
                && overflow_rectangle.intersects(dirty)
            {
                fill_rect(hdc, overflow_rectangle, crate::ui_palette::current().panel);

                frame_rect(hdc, overflow_rectangle, crate::ui_palette::current().border);

                SetTextColor(hdc, crate::ui_palette::current().text);

                styled_text(
                    hdc,
                    overflow_rectangle,
                    &format!("+{}", layout.hidden_count),
                    12,
                    700,
                    crate::ui_palette::current().text,
                    windows_sys::Win32::Graphics::Gdi::DT_CENTER,
                );
            }
        }

        if !previous_font.is_null() {
            SelectObject(hdc, previous_font);
        }

        BitBlt(
            visible,
            dirty.left,
            dirty.top,
            dirty.right - dirty.left,
            dirty.bottom - dirty.top,
            hdc,
            dirty.left,
            dirty.top,
            SRCCOPY,
        );
        windows_sys::Win32::Graphics::Gdi::GdiFlush();
        SelectObject(hdc, old_bitmap);
        DeleteObject(bitmap);
        DeleteDC(hdc);
        EndPaint(window, &paint);
    }
}

fn paint_toast(hdc: HDC, rectangle: UiRect, toast: &ReminderToast, compact: bool, title: UiRect) {
    let colors = crate::ui_palette::current();
    let hovered = compact
        && POINTER.with(|p| {
            let (x, y, _) = p.get();
            rectangle.contains(x, y)
        });
    layered_panel(
        hdc,
        rectangle,
        colors.panel,
        if hovered {
            colors.accent
        } else {
            colors.border
        },
    );
    if compact {
        content_text(
            hdc,
            rectangle.inset(6),
            &toast.name,
            13,
            600,
            colors.text,
            windows_sys::Win32::Graphics::Gdi::DT_CENTER,
        );
        return;
    }
    let icon = reminder_icon(toast.icon.as_deref());
    if let Some(icon) = icon {
        let size = 50;
        // Center across the title row (14) through the button bottom (86).
        let icon_top = rectangle.top + (14 + ACTIVE_HEIGHT - FOOTER_HEIGHT - 6 - size) / 2;
        let card = UiRect {
            left: rectangle.left + 12,
            top: icon_top,
            right: rectangle.left + 12 + size,
            bottom: icon_top + size,
        };
        layered_panel(hdc, card, colors.input, colors.border);
        if let Some(index) = asset_index(icon) {
            REMINDER_ICONS.with(|icons| {
                if let Some(asset) = &icons[index] {
                    let size = 34;
                    unsafe {
                        DrawIconEx(
                            hdc,
                            card.left + (card.right - card.left - size) / 2,
                            card.top + (card.bottom - card.top - size) / 2,
                            asset.0,
                            size,
                            size,
                            0,
                            null_mut(),
                            DI_NORMAL,
                        );
                    }
                }
            });
        } else {
            draw_text(
                hdc,
                card,
                icon,
                ("Segoe UI", 32, 400),
                colors.accent,
                windows_sys::Win32::Graphics::Gdi::DT_CENTER,
            );
        }
    }
    content_text(
        hdc,
        title,
        &toast.name,
        15,
        600,
        colors.text,
        windows_sys::Win32::Graphics::Gdi::DT_CENTER,
    );
}

// Known names select embedded image resources; other strings remain custom glyphs.
fn reminder_icon(icon: Option<&str>) -> Option<&str> {
    match icon.map(str::trim).filter(|s| !s.is_empty())? {
        "stand" | "stand_up" | "move" => Some("stand_up"),
        "eyes" | "eye_rest" => Some("eye_rest"),
        other => Some(other),
    }
}
fn asset_index(name: &str) -> Option<usize> {
    ["water", "stand_up", "stretch", "eye_rest", "generic"]
        .iter()
        .position(|n| *n == name)
}

fn layered_panel(dc: HDC, rect: UiRect, fill: u32, border: u32) {
    unsafe {
        let colors = crate::ui_palette::current();
        // Opaque, preblended halo inside the allocated bounds: no out-of-region redraw.
        for (inset, strength) in [(0, 18), (1, 35), (2, 65), (3, 100)] {
            let edge = blend(colors.background, border, strength);
            glow_round(dc, rect.inset(inset), fill, edge, 16 - inset * 2);
        }
        glow_round(dc, rect.inset(4), fill, blend(fill, border, 25), 8);
        for y in 0..3 {
            fill_rect(
                dc,
                UiRect {
                    left: rect.left + 9,
                    right: rect.right - 9,
                    top: rect.top + 5 + y,
                    bottom: rect.top + 6 + y,
                },
                blend(fill, border, 18 - y as u32 * 5),
            );
        }
    }
}

unsafe fn glow_round(dc: HDC, rect: UiRect, fill: u32, edge: u32, radius: i32) {
    use windows_sys::Win32::Graphics::Gdi::{CreatePen, PS_SOLID, RoundRect};
    unsafe {
        let brush = CreateSolidBrush(fill);
        let pen = CreatePen(PS_SOLID, 1, edge);
        let old_brush = SelectObject(dc, brush);
        let old_pen = SelectObject(dc, pen);
        RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius,
            radius,
        );
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        DeleteObject(pen);
        DeleteObject(brush);
    }
}
fn blend(a: u32, b: u32, percent: u32) -> u32 {
    let channel = |shift: u32| {
        (((a >> shift) & 255) * (100 - percent) + ((b >> shift) & 255) * percent) / 100
    };
    channel(0) | channel(8) << 8 | channel(16) << 16
}
fn styled_text(dc: HDC, rect: UiRect, text: &str, size: i32, weight: i32, color: u32, align: u32) {
    let family = match weight {
        500 => "Oxanium Medium",
        600 => "Oxanium SemiBold",
        _ => "Oxanium",
    };
    draw_text(dc, rect, text, (family, size, weight), color, align);
}
fn content_text(dc: HDC, rect: UiRect, text: &str, size: i32, weight: i32, color: u32, align: u32) {
    draw_text(
        dc,
        rect,
        text,
        ("Rajdhani SemiBold", size, weight),
        color,
        align,
    );
}
fn draw_text(
    dc: HDC,
    rect: UiRect,
    text: &str,
    font_spec: (&str, i32, i32),
    color: u32,
    align: u32,
) {
    let (family, size, weight) = font_spec;
    use windows_sys::Win32::Graphics::Gdi::*;
    unsafe {
        let font = CreateFontW(
            -size,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            ANTIALIASED_QUALITY as u32,
            0,
            wide(family).as_ptr(),
        );
        let old = if font.is_null() {
            null_mut()
        } else {
            SelectObject(dc, font)
        };
        SetTextColor(dc, color);
        let text = wide(text);
        let mut r = rect.as_windows_rect();
        DrawTextW(
            dc,
            text.as_ptr(),
            (text.len() - 1) as i32,
            &mut r,
            align | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );

        if !font.is_null() {
            SelectObject(dc, old);
            DeleteObject(font);
        }
    }
}

fn handle_click(lparam: LPARAM) {
    let snapshot = current_snapshot();

    let Some(active) = snapshot.pending.first() else {
        return;
    };

    let layout = Layout::for_snapshot(&snapshot);

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
    fn inset(self, n: i32) -> Self {
        Self {
            left: self.left + n,
            top: self.top + n,
            right: self.right - n,
            bottom: self.bottom - n,
        }
    }
    fn from_windows(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
    fn intersects(self, r: Self) -> bool {
        self.left < r.right && self.right > r.left && self.top < r.bottom && self.bottom > r.top
    }
    fn inside(self, r: Self) -> bool {
        self.left >= r.left && self.right <= r.right && self.top >= r.top && self.bottom <= r.bottom
    }

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

    title_rect: UiRect,
    timer_rect: UiRect,
    footer: UiRect,
    done: UiRect,
    snooze: UiRect,
}

impl Layout {
    fn for_snapshot(snapshot: &ReminderSnapshot) -> Self {
        let mut layout = Self::new(snapshot.pending.len());
        if snapshot
            .pending
            .first()
            .is_some_and(|t| reminder_icon(t.icon.as_deref()).is_some())
        {
            layout.arrange_row(76);
        }
        layout
    }

    fn arrange_row(&mut self, left: i32) {
        let gap = 8;
        let available = WIDTH - 16 - left - gap;
        self.done.left = left;
        self.done.right = left + available * 2 / 3;
        self.snooze.left = self.done.right + gap;
        self.title_rect.left = self.done.left;
        self.title_rect.right = self.done.right;
        self.timer_rect.left = self.snooze.left;
        self.timer_rect.right = self.snooze.right;
    }
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

        let button_top = active.bottom - FOOTER_HEIGHT - 42;

        let done = UiRect {
            left: active.left + 16,
            top: button_top,
            right: active.left + 112,
            bottom: active.bottom - FOOTER_HEIGHT - 6,
        };

        let snooze = UiRect {
            left: WIDTH - 124,
            top: button_top,
            right: WIDTH - 16,
            bottom: active.bottom - FOOTER_HEIGHT - 6,
        };

        let mut layout = Self {
            width: WIDTH,
            height,

            active,

            previews,

            overflow,
            hidden_count,

            title_rect: UiRect {
                left: 0,
                top: active.top + 14,
                right: 0,
                bottom: active.top + 44,
            },
            timer_rect: UiRect {
                left: 202,
                top: active.top + 14,
                right: WIDTH - 16,
                bottom: active.top + 44,
            },
            footer: UiRect {
                left: 4,
                top: active.bottom - FOOTER_HEIGHT,
                right: WIDTH - 4,
                bottom: active.bottom - 4,
            },
            done,
            snooze,
        };
        layout.arrange_row(16);
        layout
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

fn invalidate(window: HWND, rect: UiRect) {
    unsafe {
        InvalidateRect(window, &rect.as_windows_rect(), 0);
    }
}
fn update_pointer(window: HWND, next: (i32, i32, bool)) {
    let old = POINTER.with(|p| p.replace(next));
    let layout = Layout::for_snapshot(&current_snapshot());
    // Compare targets/states, not raw mouse coordinates.
    for rect in [layout.done, layout.snooze]
        .into_iter()
        .chain(layout.previews.iter().map(|(_, r)| *r))
    {
        let state = |p: (i32, i32, bool)| (rect.contains(p.0, p.1), rect.contains(p.0, p.1) && p.2);
        if state(old) != state(next) {
            invalidate(window, rect);
        }
    }
}
fn interval_text(minutes: u64) -> String {
    match minutes {
        1 => "every 1 minute".into(),
        0..60 => format!("every {minutes} minutes"),
        60 => "every 1 hour".into(),
        n if n % 60 == 0 => format!("every {} hours", n / 60),
        n => format!("every {:02}:{:02} hours", n / 60, n % 60),
    }
}
fn paint_footer(dc: HDC, rect: UiRect, minutes: u64) {
    let colors = crate::ui_palette::current();
    unsafe {
        crate::ui_theme::rounded(dc, rect.as_windows_rect(), colors.background, colors.border);
        fill_rect(
            dc,
            UiRect {
                bottom: rect.top + 1,
                ..rect
            },
            colors.border,
        );
        BRAND.with(|icon| {
            if let Some(icon) = icon {
                DrawIconEx(
                    dc,
                    10,
                    rect.top + (rect.bottom - rect.top - 16) / 2,
                    icon.0,
                    16,
                    16,
                    0,
                    null_mut(),
                    DI_NORMAL,
                );
            }
        });
        styled_text(
            dc,
            UiRect {
                left: 31,
                top: rect.top,
                right: 65,
                bottom: rect.bottom,
            },
            "DAT",
            12,
            600,
            colors.accent,
            0,
        );
        styled_text(
            dc,
            UiRect {
                left: 66,
                top: rect.top,
                right: rect.right - 10,
                bottom: rect.bottom,
            },
            &interval_text(minutes),
            11,
            500,
            colors.muted,
            DT_RIGHT,
        );
    }
}

#[cfg(test)]
pub fn verify_hover_stability(window: HWND) {
    use windows_sys::Win32::{
        Graphics::Gdi::{GetDC, GetPixel, ReleaseDC},
        UI::WindowsAndMessaging::{SendMessageW, WM_MOUSEMOVE},
    };
    unsafe {
        use windows_sys::Win32::Graphics::Gdi::*;
        let dc = GetDC(window);
        for (family, weight) in [
            ("Rajdhani SemiBold", 600),
            ("Oxanium", 400),
            ("Oxanium Medium", 500),
            ("Oxanium SemiBold", 600),
            ("Oxanium", 700),
        ] {
            let font = CreateFontW(
                -12,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                0,
                0,
                ANTIALIASED_QUALITY as u32,
                0,
                wide(family).as_ptr(),
            );
            assert!(!font.is_null());
            let old = SelectObject(dc, font);
            let mut name = [0u16; 64];
            let length = GetTextFaceW(dc, name.len() as i32, name.as_mut_ptr());
            assert!(length > 0);
            let name = String::from_utf16_lossy(
                &name[..name.iter().position(|&n| n == 0).unwrap_or(name.len())],
            );
            assert_eq!(name, family, "GDI selected a fallback font");
            let mut metrics: TEXTMETRICW = std::mem::zeroed();
            assert_ne!(GetTextMetricsW(dc, &mut metrics), 0);
            assert_eq!(metrics.tmWeight, weight);
            SelectObject(dc, old);
            DeleteObject(font);
        }
        ReleaseDC(window, dc);
    }
    REMINDER_ICONS.with(|icons| {
        assert!(
            icons.iter().all(Option::is_some),
            "embedded reminder assets failed to load"
        )
    });
    let layout = Layout::for_snapshot(&current_snapshot());
    for (name, rect) in [("Done", layout.done), ("Snooze", layout.snooze)] {
        let position = (((rect.top + 10) as isize) << 16) | (rect.left + 10) as isize;
        unsafe {
            SendMessageW(window, WM_MOUSEMOVE, 0, position);
            SendMessageW(window, WM_PAINT, 0, 0);
        }
        let pixels = || unsafe {
            let dc = GetDC(window);
            let result: Vec<_> = (rect.top..rect.bottom)
                .flat_map(|y| (rect.left..rect.right).map(move |x| GetPixel(dc, x, y)))
                .collect();
            ReleaseDC(window, dc);
            result
        };
        for _ in 0..15 {
            thread::sleep(std::time::Duration::from_secs(1));
            // Establish identical input state before measuring a timer-only repaint.
            // Actual desktop pointer movement is outside this deterministic check.
            unsafe {
                SendMessageW(window, WM_MOUSEMOVE, 0, position);
                SendMessageW(window, WM_PAINT, 0, 0);
            }
            let before = pixels();
            unsafe {
                SendMessageW(window, WM_TIMER, 1, 0);
                SendMessageW(window, WM_PAINT, 0, 0);
            }
            assert!(
                pixels() == before,
                "{name} changed during a timer-only repaint"
            );
        }
        println!("{name}: 15 timer updates with stable simulated hover pixels");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn button_columns_align_title_and_timer_with_two_to_one_widths() {
        for count in 1..6 {
            for left in [16, 76] {
                let mut layout = Layout::new(count);
                layout.arrange_row(left);
                assert_eq!(layout.title_rect.left, layout.done.left);
                assert_eq!(layout.title_rect.right, layout.done.right);
                assert_eq!(layout.timer_rect.left, layout.snooze.left);
                assert_eq!(layout.timer_rect.right, layout.snooze.right);
                let done_width = layout.done.right - layout.done.left;
                let snooze_width = layout.snooze.right - layout.snooze.left;
                assert!((done_width - 2 * snooze_width).abs() <= 2);
                assert_eq!(layout.snooze.left - layout.done.right, 8);
                assert!(!layout.timer_rect.intersects(layout.snooze));
            }
        }
    }
    #[test]
    fn icon_defaults_and_aliases() {
        assert_eq!(reminder_icon(None), None);
        assert_eq!(reminder_icon(Some("  ")), None);
        assert_eq!(
            reminder_icon(Some("stand")),
            reminder_icon(Some("stand_up"))
        );
        assert_eq!(reminder_icon(Some("eyes")), reminder_icon(Some("eye_rest")));
        for name in ["water", "stand_up", "stretch", "eye_rest", "generic"] {
            assert!(asset_index(reminder_icon(Some(name)).unwrap()).is_some());
        }
    }
    #[test]
    fn footer_intervals_and_dynamic_regions() {
        for (minutes, expected) in [
            (1, "every 1 minute"),
            (5, "every 5 minutes"),
            (45, "every 45 minutes"),
            (60, "every 1 hour"),
            (120, "every 2 hours"),
            (75, "every 01:15 hours"),
            (90, "every 01:30 hours"),
            (135, "every 02:15 hours"),
        ] {
            assert_eq!(interval_text(minutes), expected);
        }
        for count in 1..8 {
            let l = Layout::new(count);
            assert!(l.timer_rect.inside(l.active));
            assert!(!l.timer_rect.intersects(l.done));
            assert!(!l.timer_rect.intersects(l.footer));
            assert!(l.done.bottom < l.footer.top);
            assert!(l.snooze.bottom < l.footer.top);
            assert_eq!(l.footer.bottom, l.height - 4);
        }
    }
    #[test]
    fn stacked_layout_preserves_active_buttons_previews_and_overflow() {
        let single = Layout::new(1);
        assert!(single.previews.is_empty());
        assert!(single.overflow.is_none());
        let many = Layout::new(5);
        assert_eq!(many.previews.len(), 2);
        assert_eq!(many.hidden_count, 2);
        assert!(many.overflow.is_some());
        assert!(many.done.top >= many.active.top + 48);
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
