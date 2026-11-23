//! Image-to-ASCII background art.
//!
//! This is the Rust port of `preview_ascii.py`: an image is reduced to an ink
//! density map, contrast-stretched (with a percentile cutoff so a flat colored
//! background lands on pure black instead of leaving grain), median-filtered,
//! resized to the character grid and mapped through a dark-to-dense ramp.
//!
//! The art is painted as a BACKGROUND layer: only the glyphs (and their
//! foreground color) are written, never a background style, so every component
//! rendered afterwards keeps its own fill and stays legible. The layer is
//! centered in the area it is given, capped to it (aspect-preserving), and can
//! be nudged by a cell offset from `/wallpaper image.png [x,y]`. The shipped
//! artwork is pinned to the bottom-left corner instead, sitting flush with the
//! left side of the viewport and the last row of the chat.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use image::{DynamicImage, GrayImage, imageops};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

/// Dark-to-dense ramp (the script's `--mode ramp` default).
pub const DEFAULT_RAMP: &str = " .:-=+*#%@";

/// Percent of pixels trimmed at each end before stretching the range. The
/// approved artwork uses 10: it flattens the source's blue background while
/// keeping the busts' interior gradient.
const CUTOFF_PERCENT: f64 = 10.0;

/// Terminal cells are about twice as tall as they are wide.
const CELL_ASPECT: f64 = 0.5;

/// The default artwork ("ajinho"), shipped with the binary.
const DEFAULT_ARTWORK: &[u8] = include_bytes!("../../assets/wallpaper.jpg");

/// One rendered grid, cached until the available area changes.
struct Art {
    max_cols: u16,
    max_rows: u16,
    cols: u16,
    rows: u16,
    lines: Vec<String>,
}

/// Placement of the art inside its area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Center,
    BottomLeft,
}

/// A wallpaper: the prepared luminance map plus an optional cell offset.
pub struct Wallpaper {
    source: GrayImage,
    offset: (i32, i32),
    anchor: Anchor,
    cache: Option<Art>,
}

impl Wallpaper {
    /// The artwork embedded in the binary, used when no `/wallpaper` has been
    /// issued and by `/wallpaper` with no arguments. Decoded once per process.
    pub fn embedded() -> Result<Self, String> {
        static SOURCE: OnceLock<Result<GrayImage, String>> = OnceLock::new();
        let source = SOURCE.get_or_init(|| {
            let image = image::load_from_memory(DEFAULT_ARTWORK)
                .map_err(|error| format!("embedded wallpaper: {error}"))?;
            Ok(prepare(image))
        });
        match source {
            Ok(source) => Ok(Self {
                source: source.clone(),
                offset: (0, 0),
                anchor: Anchor::BottomLeft,
                cache: None,
            }),
            Err(error) => Err(error.clone()),
        }
    }

    /// Decode `path` and prepare it for the ramp. Supports every format the
    /// `image` crate is built with (png/jpeg/gif/webp).
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let image = image::load_from_memory(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Self {
            source: prepare(image),
            offset: (0, 0),
            anchor: Anchor::Center,
            cache: None,
        })
    }

    /// Shift the centered art by `offset` terminal cells (positive = right /
    /// down). The centering itself is unchanged, so `(0, 0)` is the default.
    pub fn with_offset(mut self, offset: (i32, i32)) -> Self {
        self.offset = offset;
        self
    }

    /// Paint the art behind `area`. Glyph cells get `fg`; the background style
    /// already on the buffer (and every cell the art leaves blank) is kept, so
    /// components drawn later are unaffected apart from the ink they cover.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect, fg: Color) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.ensure_art(area.width, area.height);
        let Some(art) = &self.cache else {
            return;
        };
        let x0 = area.x as i32
            + match self.anchor {
                Anchor::Center => (area.width as i32 - art.cols as i32) / 2,
                Anchor::BottomLeft => 0,
            }
            + self.offset.0;
        let y0 = area.y as i32
            + match self.anchor {
                Anchor::Center => (area.height as i32 - art.rows as i32) / 2,
                Anchor::BottomLeft => area.height as i32 - art.rows as i32,
            }
            + self.offset.1;
        let style = Style::default().fg(fg);
        for (row, line) in art.lines.iter().enumerate() {
            let y = y0 + row as i32;
            if y < area.y as i32 || y >= area.bottom() as i32 {
                continue;
            }
            for (col, ch) in line.chars().enumerate() {
                if ch == ' ' {
                    continue;
                }
                let x = x0 + col as i32;
                if x < area.x as i32 || x >= area.right() as i32 {
                    continue;
                }
                if let Some(cell) = buf.cell_mut((x as u16, y as u16)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
            }
        }
    }

    /// Rebuild the grid only when the available area changed. The art is
    /// contained (never cropped) and keeps the source aspect ratio.
    fn ensure_art(&mut self, max_cols: u16, max_rows: u16) {
        if self
            .cache
            .as_ref()
            .is_some_and(|art| art.max_cols == max_cols && art.max_rows == max_rows)
        {
            return;
        }
        let (cols, rows) = fit(
            self.source.width(),
            self.source.height(),
            max_cols,
            max_rows,
        );
        let resized = imageops::resize(
            &self.source,
            u32::from(cols),
            u32::from(rows),
            imageops::FilterType::Lanczos3,
        );
        self.cache = Some(Art {
            max_cols,
            max_rows,
            cols,
            rows,
            lines: ramp_lines(&resized, DEFAULT_RAMP),
        });
    }
}

/// The biggest grid of at most `max_cols` x `max_rows` that keeps the source
/// aspect ratio under the terminal cell aspect.
fn fit(src_w: u32, src_h: u32, max_cols: u16, max_rows: u16) -> (u16, u16) {
    if src_w == 0 || src_h == 0 || max_cols == 0 || max_rows == 0 {
        return (1, 1);
    }
    let mut cols = u32::from(max_cols);
    let mut rows = ((f64::from(src_h) * f64::from(cols) / f64::from(src_w) * CELL_ASPECT).round()
        as u32)
        .max(1);
    if rows > u32::from(max_rows) {
        rows = u32::from(max_rows);
        cols = ((f64::from(src_w) * f64::from(rows) / f64::from(src_h) / CELL_ASPECT).round()
            as u32)
            .max(1)
            .min(u32::from(max_cols));
    }
    (cols as u16, rows as u16)
}

/// Decode → grayscale → autocontrast → median. Mirrors the script's
/// `load_luminance` + `--median` order: contrast is stretched on the full
/// resolution image, then speckle is removed, then the grid is resampled.
fn prepare(image: DynamicImage) -> GrayImage {
    let gray = luminance(&image);
    let stretched = autocontrast(&gray, CUTOFF_PERCENT);
    median3(&stretched)
}

/// PIL's `convert("L")` coefficients (ITU-R 601-2), truncating like Pillow.
/// Transparent pixels are composited over black first: no ink, like the
/// terminal background.
fn luminance(image: &DynamicImage) -> GrayImage {
    let rgba = image.to_rgba8();
    GrayImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let pixel = rgba.get_pixel(x, y);
        let [r, g, b, a] = pixel.0;
        let (r, g, b) = (
            u32::from(r) * u32::from(a) / 255,
            u32::from(g) * u32::from(a) / 255,
            u32::from(b) * u32::from(a) / 255,
        );
        let luma = (r * 299 + g * 587 + b * 114) / 1000;
        image::Luma([luma as u8])
    })
}

/// `ImageOps.autocontrast(cutoff)`: trim `cutoff_percent` of the pixels from
/// each end of the histogram, then stretch what is left over the full range.
fn autocontrast(gray: &GrayImage, cutoff_percent: f64) -> GrayImage {
    let mut histogram = [0f64; 256];
    for pixel in gray.pixels() {
        histogram[pixel.0[0] as usize] += 1.0;
    }
    let total: f64 = histogram.iter().sum();
    if total == 0.0 {
        return gray.clone();
    }
    let mut cut = total * cutoff_percent.clamp(0.0, 49.0) / 100.0;
    for count in &mut histogram {
        if cut > *count {
            cut -= *count;
            *count = 0.0;
        } else {
            *count -= cut;
            break;
        }
    }
    let mut cut = total * cutoff_percent.clamp(0.0, 49.0) / 100.0;
    for count in histogram.iter_mut().rev() {
        if cut > *count {
            cut -= *count;
            *count = 0.0;
        } else {
            *count -= cut;
            break;
        }
    }
    let Some(low) = histogram.iter().position(|count| *count > 0.0) else {
        return gray.clone();
    };
    let Some(high) = histogram.iter().rposition(|count| *count > 0.0) else {
        return gray.clone();
    };
    if high <= low {
        return gray.clone();
    }
    let scale = 255.0 / (high - low) as f64;
    let offset = -(low as f64) * scale;
    let mut lut = [0u8; 256];
    for (value, mapped) in lut.iter_mut().enumerate() {
        *mapped = (value as f64 * scale + offset).clamp(0.0, 255.0) as u8;
    }
    GrayImage::from_fn(gray.width(), gray.height(), |x, y| {
        image::Luma([lut[gray.get_pixel(x, y).0[0] as usize]])
    })
}

/// 3x3 median filter (Pillow's `ImageFilter.MedianFilter(3)`): removes JPEG
/// speckle without softening edges. Border windows clamp to the edge pixels.
fn median3(gray: &GrayImage) -> GrayImage {
    let width = gray.width() as i64;
    let height = gray.height() as i64;
    if width < 3 || height < 3 {
        return gray.clone();
    }
    GrayImage::from_fn(gray.width(), gray.height(), |x, y| {
        let mut window = [0u8; 9];
        let mut index = 0;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let sx = (x as i64 + dx).clamp(0, width - 1) as u32;
                let sy = (y as i64 + dy).clamp(0, height - 1) as u32;
                window[index] = gray.get_pixel(sx, sy).0[0];
                index += 1;
            }
        }
        window.sort_unstable();
        image::Luma([window[4]])
    })
}

/// Map every cell to the ramp character nearest below its brightness, exactly
/// like the script: `ramp[min(last, int(value / 255 * len(ramp)))]`.
fn ramp_lines(gray: &GrayImage, ramp: &str) -> Vec<String> {
    let chars: Vec<char> = ramp.chars().collect();
    if chars.is_empty() {
        return vec![String::new(); gray.height() as usize];
    }
    let last = chars.len() - 1;
    (0..gray.height())
        .map(|y| {
            (0..gray.width())
                .map(|x| {
                    let value = gray.get_pixel(x, y).0[0] as usize;
                    chars[(value * chars.len() / 255).min(last)]
                })
                .collect()
        })
        .collect()
}

/// What `/wallpaper` asked for after parsing its argument tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// No arguments: the embedded default artwork.
    Default,
    /// `off` (or `none` / `clear`): remove the wallpaper.
    Off,
    /// A file path plus an optional cell offset from the centered position.
    Load {
        path: PathBuf,
        offset: Option<(i32, i32)>,
    },
}

/// Parse the text after `/wallpaper`. Paths with spaces must be quoted:
/// `/wallpaper "my art.png" 4,-2`. A relative path resolves against `base`
/// (the session's working directory); `~/` expands to the home directory.
pub fn parse_request(tail: &str, base: &Path) -> Result<Request, String> {
    let tail = tail.trim();
    if tail.is_empty() || matches!(tail.to_ascii_lowercase().as_str(), "default" | "reset") {
        return Ok(Request::Default);
    }
    if matches!(tail.to_ascii_lowercase().as_str(), "off" | "none" | "clear") {
        return Ok(Request::Off);
    }
    let (raw_path, rest) = split_path(tail)?;
    if raw_path.is_empty() {
        return Err("missing image path".into());
    }
    let offset = if rest.trim().is_empty() {
        None
    } else {
        Some(parse_offset(rest.trim())?)
    };
    let expanded = expand_home(raw_path);
    let path = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    Ok(Request::Load { path, offset })
}

/// Split the leading path token (quoted or up to whitespace) from the rest.
fn split_path(tail: &str) -> Result<(&str, &str), String> {
    if let Some(quoted) = tail.strip_prefix('"') {
        let Some(end) = quoted.find('"') else {
            return Err("unterminated quoted path".into());
        };
        return Ok((&quoted[..end], &quoted[end + 1..]));
    }
    match tail.find(char::is_whitespace) {
        Some(index) => Ok((&tail[..index], &tail[index..])),
        None => Ok((tail, "")),
    }
}

fn parse_offset(text: &str) -> Result<(i32, i32), String> {
    let parts: Vec<&str> = text
        .split(|ch: char| ch == ',' || ch.is_whitespace())
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() != 2 {
        return Err("offset must be `x,y` cells, e.g. `10,-3`".into());
    }
    let parse = |value: &str| {
        value
            .parse::<i32>()
            .map_err(|_| format!("`{value}` is not a whole number"))
    };
    Ok((parse(parts[0])?, parse(parts[1])?))
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(values: &[u8]) -> GrayImage {
        GrayImage::from_raw(values.len() as u32, 1, values.to_vec()).unwrap()
    }

    #[test]
    fn ramp_matches_the_python_mapping() {
        // `preview_ascii.py --width 4 --cutoff 0 --median 0 --mode ramp` on a
        // 4x1 grayscale image (all-dark, quarter, mid, white).
        let art = ramp_lines(&gray(&[0, 25, 127, 255]), DEFAULT_RAMP);
        assert_eq!(art, vec!["  =@".to_string()]);
    }

    #[test]
    fn ramp_caps_pure_white_at_the_densest_glyph() {
        assert_eq!(ramp_lines(&gray(&[255]), DEFAULT_RAMP), vec!["@"]);
    }

    #[test]
    fn autocontrast_stretches_and_trims_both_ends() {
        // 100 pixels: 10 at 20 (trimmed low), 80 in 100..=200, 10 at 240
        // (trimmed high). The remaining range 100..=200 stretches to 0..=255.
        let mut values = vec![20u8; 10];
        values.extend(100..=200);
        values.extend(vec![240u8; 10]);
        let out = autocontrast(&gray(&values), 10.0);
        assert_eq!(out.get_pixel(0, 0).0[0], 0);
        assert_eq!(out.get_pixel(60, 0).0[0], 127);
        assert_eq!(out.get_pixel(120, 0).0[0], 255);
    }

    #[test]
    fn autocontrast_without_cutoff_uses_the_observed_range() {
        let out = autocontrast(&gray(&[10, 10, 20, 30]), 0.0);
        assert_eq!(
            out.pixels().map(|p| p.0[0]).collect::<Vec<_>>(),
            vec![0, 0, 127, 255]
        );
    }

    #[test]
    fn median_removes_a_single_speckle() {
        let mut values = [100u8; 25];
        values[12] = 255;
        let out = median3(&GrayImage::from_raw(5, 5, values.to_vec()).unwrap());
        assert_eq!(out.get_pixel(2, 2).0[0], 100);
    }

    #[test]
    fn median_leaves_small_images_untouched() {
        let small = gray(&[0, 128, 255]);
        assert_eq!(median3(&small).into_raw(), small.into_raw());
    }

    #[test]
    fn fit_contains_the_source_without_cropping() {
        // Landscape source: width-limited.
        assert_eq!(fit(100, 100, 80, 40), (80, 40));
        // Tall source: height-limited, narrow grid.
        assert_eq!(fit(10, 100, 20, 20), (4, 20));
        // Degenerate areas never divide by zero.
        assert_eq!(fit(10, 10, 0, 10), (1, 1));
    }

    #[test]
    fn render_centers_and_clips_the_art() {
        let mut wallpaper = Wallpaper {
            source: gray(&[255]),
            offset: (0, 0),
            anchor: Anchor::Center,
            cache: Some(Art {
                max_cols: 10,
                max_rows: 4,
                cols: 3,
                rows: 2,
                lines: vec!["abc".into(), "def".into()],
            }),
        };
        let area = Rect::new(0, 0, 10, 4);
        let mut buf = Buffer::empty(area);
        wallpaper.render(&mut buf, area, Color::Red);
        // Centered: x = (10-3)/2 = 3, y = (4-2)/2 = 1.
        assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "a");
        assert_eq!(buf.cell((5, 2)).unwrap().symbol(), "f");
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), " ");

        let mut shifted_buf = Buffer::empty(area);

        let mut shifted = Wallpaper {
            source: gray(&[255]),
            offset: (20, 20),
            anchor: Anchor::Center,
            cache: None,
        };
        // Reuse a tiny source so the cache rebuild stays cheap.
        shifted.source = GrayImage::new(2, 2);
        shifted.cache = Some(Art {
            max_cols: 10,
            max_rows: 4,
            cols: 3,
            rows: 2,
            lines: vec!["abc".into(), "def".into()],
        });
        shifted.render(&mut shifted_buf, area, Color::Red);
        assert!(shifted_buf.cell((3, 1)).unwrap().symbol() != "a");
    }

    #[test]
    fn render_pins_the_default_anchor_to_the_bottom_left() {
        let mut wallpaper = Wallpaper {
            source: gray(&[255]),
            offset: (0, 0),
            anchor: Anchor::BottomLeft,
            cache: Some(Art {
                max_cols: 10,
                max_rows: 4,
                cols: 3,
                rows: 2,
                lines: vec!["abc".into(), "def".into()],
            }),
        };
        let area = Rect::new(0, 0, 10, 4);
        let mut buf = Buffer::empty(area);
        wallpaper.render(&mut buf, area, Color::Red);
        // Bottom-left: x = 0, y = 4 - 2 = 2.
        assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "a");
        assert_eq!(buf.cell((2, 3)).unwrap().symbol(), "f");
        assert_eq!(buf.cell((1, 2)).unwrap().symbol(), "b");
    }

    #[test]
    fn the_embedded_art_is_pinned_to_the_bottom_left() {
        assert_eq!(Wallpaper::embedded().unwrap().anchor, Anchor::BottomLeft);
    }

    #[test]
    fn parse_request_defaults_to_the_embedded_art() {
        assert_eq!(
            parse_request("", Path::new("/tmp")).unwrap(),
            Request::Default
        );
        assert_eq!(
            parse_request("  reset ", Path::new("/tmp")).unwrap(),
            Request::Default
        );
    }

    #[test]
    fn parse_request_reads_path_and_offset() {
        assert_eq!(
            parse_request("art.png 10,-3", Path::new("/repo")).unwrap(),
            Request::Load {
                path: PathBuf::from("/repo/art.png"),
                offset: Some((10, -3)),
            }
        );
        assert_eq!(
            parse_request("art.png 10 -3", Path::new("/repo")).unwrap(),
            Request::Load {
                path: PathBuf::from("/repo/art.png"),
                offset: Some((10, -3)),
            }
        );
        assert_eq!(
            parse_request("/abs/art.png", Path::new("/repo")).unwrap(),
            Request::Load {
                path: PathBuf::from("/abs/art.png"),
                offset: None,
            }
        );
    }

    #[test]
    fn parse_request_supports_quoted_paths() {
        assert_eq!(
            parse_request("\"my art.png\" 2,1", Path::new("/repo")).unwrap(),
            Request::Load {
                path: PathBuf::from("/repo/my art.png"),
                offset: Some((2, 1)),
            }
        );
        assert!(parse_request("\"my art.png", Path::new("/repo")).is_err());
    }

    #[test]
    fn parse_request_rejects_a_malformed_offset_but_accepts_off() {
        assert!(parse_request("art.png 1,2,3", Path::new("/repo")).is_err());
        assert!(parse_request("art.png x,2", Path::new("/repo")).is_err());
        assert_eq!(
            parse_request("OFF", Path::new("/repo")).unwrap(),
            Request::Off
        );
    }

    #[test]
    fn embedded_artwork_decodes_and_renders() {
        let mut wallpaper = Wallpaper::embedded().expect("artwork must decode");
        let area = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(area);
        wallpaper.render(&mut buf, area, Color::Red);
        assert!(
            buf.content.iter().any(|cell| cell.symbol() != " "),
            "the default wallpaper must paint some ink"
        );
    }
}
