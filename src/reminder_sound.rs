use std::path::Path;

use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};

pub fn play(path: &Path) {
    let wide_path = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    unsafe {
        if PlaySoundW(
            wide_path.as_ptr(),
            std::ptr::null_mut(),
            SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
        ) == 0
        {
            eprintln!(
                "Could not play reminder sound {} ; the reminder remains available.",
                path.display()
            );
        }
    }
}
