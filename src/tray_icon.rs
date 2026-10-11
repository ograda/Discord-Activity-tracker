//! Native tray icon composition. The normal embedded helmet is never modified.
use anyhow::{Result, bail};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Graphics::Gdi::*, System::LibraryLoader::GetModuleHandleW, UI::WindowsAndMessaging::*,
};

pub struct TrayIcon(pub HICON);
impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            DestroyIcon(self.0);
        }
    }
}
impl TrayIcon {
    pub fn load(size: i32) -> Result<Self> {
        unsafe {
            // Without LR_SHARED, both LoadImage handles belong to us.
            let mut icon = LoadImageW(
                GetModuleHandleW(null()),
                std::ptr::without_provenance::<u16>(1),
                IMAGE_ICON,
                size,
                size,
                0,
            ) as HICON;
            if icon.is_null() {
                eprintln!("Could not load embedded DAT icon; using Windows fallback icon.");
                icon = LoadImageW(null_mut(), IDI_APPLICATION, IMAGE_ICON, size, size, 0) as HICON;
            }
            if icon.is_null() {
                bail!(
                    "Could not load tray icon: {}",
                    std::io::Error::last_os_error()
                );
            }
            Ok(Self(icon))
        }
    }
    pub fn muted(&self, size: i32) -> Result<Self> {
        with_icon_pixels(self.0, size, |pixels, color| {
            overlay_slash(pixels, size as usize);
            // 32-bit alpha determines transparency. Supply a zeroed monochrome
            // mask as required by CreateIconIndirect, including row alignment.
            let mask_bits = vec![0u8; ((size as usize).div_ceil(16) * 2) * size as usize];
            unsafe {
                let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr() as _);
                if mask.is_null() {
                    bail!("Could not create tray icon mask");
                }
                let info = ICONINFO {
                    fIcon: 1,
                    xHotspot: 0,
                    yHotspot: 0,
                    hbmMask: mask,
                    hbmColor: color,
                };
                let icon = CreateIconIndirect(&info);
                DeleteObject(mask as _);
                if icon.is_null() {
                    bail!("Could not create muted DAT icon");
                }
                Ok(Self(icon))
            }
        })
    }
}

// Allocate one temporary top-down alpha DIB, draw the source, and release every
// GDI object on success or error. Icons themselves are cached for the tray lifetime.
fn with_icon_pixels<T>(
    icon: HICON,
    size: i32,
    use_pixels: impl FnOnce(&mut [u32], HBITMAP) -> Result<T>,
) -> Result<T> {
    if !(1..=256).contains(&size) {
        bail!("Invalid tray icon size {size}");
    }
    unsafe {
        let dc = CreateCompatibleDC(null_mut());
        if dc.is_null() {
            bail!("Could not create tray icon DC");
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            ..std::mem::zeroed()
        };
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bitmap.is_null() {
            DeleteDC(dc);
            bail!("Could not create tray icon bitmap");
        }
        let previous = SelectObject(dc, bitmap as _);
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
        pixels.fill(0);
        let drawn = DrawIconEx(dc, 0, 0, icon, size, size, 0, null_mut(), DI_NORMAL);
        GdiFlush();
        SelectObject(dc, previous);
        let result = if drawn == 0 {
            Err(anyhow::anyhow!("Could not draw DAT icon"))
        } else {
            use_pixels(pixels, bitmap)
        };
        DeleteObject(bitmap as _);
        DeleteDC(dc);
        result
    }
}
fn overlay_slash(pixels: &mut [u32], size: usize) {
    let size_f = size as f32;
    // Lower-right diagonal badge leaves the helmet and crest recognizable.
    let a = (size_f * 0.56, size_f * 0.90);
    let b = (size_f * 0.90, size_f * 0.56);
    let radius = (size_f * 0.06).max(0.85);
    let outline = radius + (size_f * 0.035).max(0.65);
    for y in 0..size {
        for x in 0..size {
            let dx = b.0 - a.0;
            let dy = b.1 - a.1;
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let t = (((px - a.0) * dx + (py - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            let distance = ((px - a.0 - t * dx).powi(2) + (py - a.1 - t * dy).powi(2)).sqrt();
            let pixel = &mut pixels[y * size + x];
            blend(
                pixel,
                0x00f4f4f4,
                (outline + 0.5 - distance).clamp(0.0, 1.0),
            );
            blend(pixel, 0x00ed3038, (radius + 0.5 - distance).clamp(0.0, 1.0));
        }
    }
}
fn blend(pixel: &mut u32, color: u32, coverage: f32) {
    let alpha = (coverage * 255.0).round() as u32;
    let inverse = 255 - alpha;
    let mut result = (alpha + ((*pixel >> 24) * inverse + 127) / 255) << 24;
    for shift in [0, 8, 16] {
        let channel =
            (((color >> shift) & 255) * alpha + ((*pixel >> shift) & 255) * inverse + 127) / 255;
        result |= channel << shift;
    }
    *pixel = result;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slash_is_visible_at_small_sizes_and_leaves_most_of_logo_untouched() {
        for size in [16, 20, 24, 32, 48, 64] {
            let mut pixels = vec![0xff806020; size * size];
            overlay_slash(&mut pixels, size);
            let changed = pixels.iter().filter(|p| **p != 0xff806020).count();
            assert!(changed > size);
            assert!(changed < size * size / 3);
            assert!(
                pixels
                    .iter()
                    .any(|p| (*p >> 16) & 255 > 220 && (*p >> 8) & 255 < 80)
            );
            assert_eq!(pixels[0], 0xff806020);
        }
    }
    #[test]
    fn native_icons_compose_at_notification_area_sizes() {
        for size in [16, 20, 24, 32, 48, 64] {
            let normal = TrayIcon::load(size).unwrap();
            let muted = normal.muted(size).unwrap();
            assert_ne!(normal.0, muted.0);
            let before = with_icon_pixels(normal.0, size, |pixels, _| Ok(pixels.to_vec())).unwrap();
            let after = with_icon_pixels(muted.0, size, |pixels, _| Ok(pixels.to_vec())).unwrap();
            assert_ne!(before, after);
            assert!(after.iter().any(|p| *p >> 24 == 0));
        }
    }
}
