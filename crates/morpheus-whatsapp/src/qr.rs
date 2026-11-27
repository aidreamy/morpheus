//! Terminal rendering of a pairing payload.

use qrcode::QrCode;
use qrcode::render::unicode;

use crate::error::{Error, Result};

/// Render a pairing payload as a scannable terminal QR code.
///
/// The output uses half-block characters (`▀`/`▄`/`█`), so two QR rows per
/// terminal row - a standard 80-column terminal fits the payload with the
/// quiet zone. Print it with a monospace font and a dark-on-light terminal;
/// if the user's theme is inverted, [`render_inverted`] flips the colors.
///
/// ```no_run
/// # fn demo(payload: &str) -> morpheus_whatsapp::Result<()> {
/// println!("{}", morpheus_whatsapp::qr::render(payload)?);
/// # Ok(())
/// # }
/// ```
pub fn render(payload: &str) -> Result<String> {
    build(payload, unicode::Dense1x2::Dark, unicode::Dense1x2::Light)
}

/// Like [`render`], but swapped for terminals with a dark background and
/// light foreground, where scanner contrast is otherwise reversed.
pub fn render_inverted(payload: &str) -> Result<String> {
    build(payload, unicode::Dense1x2::Light, unicode::Dense1x2::Dark)
}

fn build(payload: &str, dark: unicode::Dense1x2, light: unicode::Dense1x2) -> Result<String> {
    let code = QrCode::new(payload.as_bytes()).map_err(Error::qr)?;
    Ok(code
        .render::<unicode::Dense1x2>()
        .quiet_zone(true)
        .dark_color(dark)
        .light_color(light)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_whatsapp_shaped_payload() {
        // Real payloads are similar in shape and length (reference + keys).
        let payload = "2@aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789+/=,aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789+/=,aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789+/=";
        let rendered = render(payload).expect("payload fits in a QR code");

        assert!(rendered.lines().count() > 10);
        assert!(rendered.chars().any(|c| c == '█' || c == '▀' || c == '▄'));
    }

    #[test]
    fn oversized_payload_is_a_qr_error_with_a_source() {
        // The `qrcode` type stays behind the source chain instead of being
        // part of the public `Error` enum.
        let err = render(&"x".repeat(10_000)).expect_err("payload is too long");
        assert!(matches!(err, Error::Qr(_)), "{err:?}");
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn inverted_rendering_differs() {
        let payload = "2@reference";
        assert_ne!(
            render(payload).expect("qrcode"),
            render_inverted(payload).expect("qrcode")
        );
    }
}
