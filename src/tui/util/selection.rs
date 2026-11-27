use std::sync::{LazyLock, Mutex};

use arboard::Clipboard;

use crate::ui::toast::{ToastOptions, ToastState, ToastVariant};

/// Global clipboard instance, lazily initialised.
static CLIPBOARD: LazyLock<Mutex<Option<Clipboard>>> = LazyLock::new(|| Mutex::new(None));

/// Initialise the system clipboard. Call once at startup.
pub fn init_clipboard() {
    if let Ok(cb) = Clipboard::new() {
        if let Ok(mut guard) = CLIPBOARD.lock() {
            *guard = Some(cb);
        }
    } else {
        // Clipboard not available – writes will be silently ignored.
    }
}

/// Copy `text` to the system clipboard. Returns `true` on success.
pub fn copy_to_clipboard(text: &str) -> bool {
    if let Ok(mut guard) = CLIPBOARD.lock()
        && let Some(ref mut cb) = *guard
    {
        return cb.set_text(text.to_owned()).is_ok();
    }
    false
}

/// Check whether the clipboard is available.
pub fn clipboard_available() -> bool {
    if let Ok(guard) = CLIPBOARD.lock() {
        guard.is_some()
    } else {
        false
    }
}

/// Read the system clipboard's TEXT content. Returns `None` when the
/// clipboard is unavailable, the read fails, or the content is not text.
///
/// Used by the Ctrl+V paste handler (keys.rs) - the opencode mini-port: on
/// Windows the terminal usually does not emit a bracketed paste for Ctrl+V,
/// so the key event is the paste signal and the text must come from the
/// clipboard directly.
pub fn clipboard_text() -> Option<String> {
    if let Ok(mut guard) = CLIPBOARD.lock()
        && let Some(ref mut cb) = *guard
        && let Ok(text) = cb.get_text()
    {
        return Some(text);
    }
    None
}

/// Read an image from the system clipboard and encode it as PNG. Returns
/// `None` when the clipboard holds no image, since text-only clipboards are
/// handled by [`clipboard_text`], or when the pixels cannot be decoded or
/// encoded.
///
/// The Ctrl+V paste handler needs this because copying an image puts raw
/// pixels on the clipboard, often with only an HTML or text flavor. There is
/// no path to pass to `try_load_image`, so the pixels must be read here.
pub fn clipboard_image() -> Option<Vec<u8>> {
    let (width, height, rgba) = {
        let mut guard = CLIPBOARD.lock().ok()?;
        let cb = guard.as_mut()?;
        let img = cb.get_image().ok()?;
        (img.width, img.height, img.bytes.into_owned())
    };
    encode_rgba_png(width, height, rgba)
}

/// Encode raw RGBA8 clipboard pixels as PNG (the format the harness forwards
/// to the provider). Caps the image at 40 MP and the payload at 10 MB so a
/// hostile or accidental giant clipboard cannot blow up the UI thread.
fn encode_rgba_png(width: usize, height: usize, rgba: Vec<u8>) -> Option<Vec<u8>> {
    const MAX_PIXELS: usize = 40_000_000;
    const MAX_PNG_BYTES: usize = 10 * 1024 * 1024;
    if width == 0 || height == 0 || width.checked_mul(height)? > MAX_PIXELS {
        return None;
    }
    if rgba.len() < width.checked_mul(height)?.checked_mul(4)? {
        return None;
    }
    let image = image::RgbaImage::from_raw(width as u32, height as u32, rgba)?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    if png.len() > MAX_PNG_BYTES {
        return None;
    }
    Some(png)
}

/// Copy the current selection to clipboard and show a toast notification.
/// Returns `true` if text was copied.
pub fn copy_selection(text: &str, toast: &mut ToastState) -> bool {
    if text.is_empty() {
        return false;
    }

    let copied = copy_to_clipboard(text);
    let line_count = text.split('\n').count();
    let summary = if line_count > 1 {
        format!("{line_count} lines")
    } else {
        format!("{} chars", text.len())
    };

    let message = if copied {
        format!("Copied to clipboard ({summary})")
    } else {
        format!("Selected {summary}; clipboard write unavailable")
    };
    toast.show(ToastOptions {
        title: None,
        message,
        variant: ToastVariant::Info,
        duration_ms: 3000,
    });

    copied
}
