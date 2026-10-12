//! Shared presentation-only palette; no tracking or database dependencies.
use crate::settings::ColorPalette;
use std::sync::atomic::{AtomicU8, Ordering};
use windows_sys::Win32::{System::Registry::*, UI::WindowsAndMessaging::WM_APP};
pub const CHANGED: u32 = WM_APP + 49;
static SELECTED: AtomicU8 = AtomicU8::new(0);
static RESOLVED: AtomicU8 = AtomicU8::new(0);
pub const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | g << 8 | b << 16
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    pub background: u32,
    pub panel: u32,
    pub input: u32,
    pub border: u32,
    pub accent: u32,
    pub text: u32,
    pub muted: u32,
    pub primary: u32,
    pub hover: u32,
    pub pressed: u32,
    pub on_primary: u32,
    pub dark: bool,
}
pub fn colors_for(palette: ColorPalette) -> Colors {
    let mut colors = Colors {
        background: rgb(16, 14, 14),
        panel: rgb(29, 23, 23),
        input: rgb(39, 31, 30),
        border: rgb(92, 43, 34),
        accent: rgb(255, 151, 81),
        text: rgb(241, 235, 230),
        muted: rgb(179, 165, 158),
        primary: rgb(170, 57, 24),
        hover: rgb(192, 70, 29),
        pressed: rgb(133, 43, 20),
        on_primary: rgb(255, 255, 255),
        dark: true,
    };
    match palette {
        ColorPalette::Dark => {
            colors.background = rgb(19, 20, 22);
            colors.panel = rgb(29, 30, 33);
            colors.input = rgb(40, 42, 46);
            colors.border = rgb(73, 76, 81);
            colors.text = rgb(240, 241, 243);
            colors.muted = rgb(174, 178, 185);
        }
        ColorPalette::Light => {
            colors.background = rgb(242, 243, 245);
            colors.panel = rgb(255, 255, 255);
            colors.input = rgb(246, 247, 249);
            colors.border = rgb(202, 203, 207);
            colors.text = rgb(29, 30, 33);
            colors.muted = rgb(94, 96, 102);
            colors.accent = rgb(168, 57, 16);
            colors.dark = false;
        }
        _ => {}
    }
    colors
}
pub fn current() -> Colors {
    colors_for(match RESOLVED.load(Ordering::Relaxed) {
        2 => ColorPalette::Dark,
        3 => ColorPalette::Light,
        _ => ColorPalette::DatDefault,
    })
}
fn system_palette() -> ColorPalette {
    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
    let mut light = 1u32;
    let mut size = 4;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut light as *mut _ as _,
            &mut size,
        )
    };
    if result == 0 && light == 0 {
        ColorPalette::Dark
    } else {
        ColorPalette::Light
    }
}
pub fn apply(palette: ColorPalette) {
    SELECTED.store(palette as u8, Ordering::Relaxed);
    let resolved = if palette == ColorPalette::System {
        system_palette()
    } else {
        palette
    };
    RESOLVED.store(resolved as u8, Ordering::Relaxed);
    crate::settings_window::palette_changed();
    crate::statistics_window::palette_changed();
    crate::reminder_hud::palette_changed();
}
pub fn system_changed() {
    if SELECTED.load(Ordering::Relaxed) == ColorPalette::System as u8 {
        apply(ColorPalette::System);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn palettes_and_settings_round_trip() {
        let defaults: crate::settings::Settings =
            quick_xml::de::from_str("<dat-settings/>").unwrap();
        assert_eq!(defaults.color_palette, ColorPalette::DatDefault);
        for palette in ColorPalette::ALL {
            let settings = crate::settings::Settings {
                color_palette: palette,
                ..defaults.clone()
            };
            assert_eq!(
                quick_xml::de::from_str::<crate::settings::Settings>(
                    &quick_xml::se::to_string(&settings).unwrap()
                )
                .unwrap(),
                settings
            );
        }
        assert_ne!(
            colors_for(ColorPalette::DatDefault).panel,
            colors_for(ColorPalette::Dark).panel
        );
        assert!(!colors_for(ColorPalette::Light).dark);
    }
}
