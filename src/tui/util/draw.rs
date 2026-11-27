use ratatui::buffer::Buffer;
use ratatui::style::Style;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const SECTION_PAD: u16 = 2;
pub const SECTION_ITEM_INDENT: u16 = 2;

/// Wrap at spaces or split words that exceed the available display width.
pub fn wrap_words(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for word in text.split(' ') {
        let word_width = word.width();
        if word_width > max_width {
            // Unbreakable: flush what is pending, then chunk the word itself
            // at the width boundary.
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
            }
            let mut start = 0;
            let mut taken = 0usize;
            for (i, ch) in word.char_indices() {
                let char_width = ch.width().unwrap_or(0);
                if taken + char_width > max_width {
                    rows.push(word[start..i].to_string());
                    start = i;
                    taken = char_width;
                } else {
                    taken += char_width;
                }
            }
            current = word[start..].to_string();
            current_width = taken;
        } else if current_width == 0 {
            current = word.to_string();
            current_width = word_width;
        } else if current_width + 1 + word_width <= max_width {
            current.push(' ');
            current.push_str(word);
            current_width += 1 + word_width;
        } else {
            rows.push(std::mem::take(&mut current));
            current = word.to_string();
            current_width = word_width;
        }
    }
    rows.push(current);
    rows
}

pub fn draw_section_top(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    title: &str,
    fg: Style,
    muted: Style,
) {
    if width < 2 {
        return;
    }

    let inner_width = usize::from(width - 2);
    buf.set_string(x, y, format!("╭{}╮", "─".repeat(inner_width)), muted);
    if inner_width > 3 {
        let title_width = title.width().min(inner_width - 3);
        buf.set_string(x + 2, y, " ", muted);
        let (end, _) = buf.set_stringn(x + 3, y, title, title_width, fg);
        buf.set_string(end, y, " ", muted);
    }
}

pub fn draw_section_bottom(buf: &mut Buffer, x: u16, y: u16, width: u16, muted: Style) {
    if width >= 2 {
        buf.set_string(
            x,
            y,
            format!("╰{}╯", "─".repeat(usize::from(width - 2))),
            muted,
        );
    }
}

pub fn draw_section_walls(buf: &mut Buffer, x: u16, y: u16, width: u16, muted: Style) {
    if width >= 2 {
        buf.set_string(x, y, "│", muted);
        buf.set_string(x + width - 1, y, "│", muted);
    }
}

/// Write `text` into `buf` starting at (`x`, `y`), clipped to `max_w` cells.
///
/// Control characters are skipped: writing them into buffer cells makes
/// ratatui's buffer diff panic ("control character passed to cell_width
/// without filtering"), and every text this renders ultimately derives from
/// model output, session titles or user input that can carry `\n`, `\t`,
/// `\r` or raw ANSI bytes.
///
/// Coordinate math is overflow-checked: if `x + max_w` (or `x + i`) would
/// wrap past `u16::MAX` the remaining text is clipped, mirroring the
/// majority of the originals this replaced.
pub fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let Some(right) = x.checked_add(max_w) else {
        return;
    };
    for (i, ch) in text.chars().enumerate() {
        if ch.is_control() {
            continue;
        }
        let Some(cx) = x.checked_add(i as u16) else {
            break;
        };
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// Compact variant of [`draw_text_line`]: control characters are dropped
/// BEFORE column assignment (`filter().enumerate()`), so unlike
/// [`draw_text_line`] they leave no gap — `"\nABC"` starts `ABC` at `x`.
/// This is the dialog/slash-menu semantics: their labels come from
/// curated/static strings where a gap would be a visible artifact.
pub fn draw_text_line_compact(
    buf: &mut Buffer,
    text: &str,
    x: u16,
    y: u16,
    max_w: u16,
    style: Style,
) {
    let right = x + max_w;
    for (i, ch) in text.chars().filter(|c| !c.is_control()).enumerate() {
        let cx = x + i as u16;
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// Wide-glyph-aware variant of [`draw_text_line`]: CJK / emoji characters
/// occupy two terminal cells, so advancing one cell per char would shift
/// right-anchored segments and overwrite the wide char's second half. Used
/// for footer rows whose text is expected to contain wide glyphs.
pub fn draw_text_line_wide(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    let mut cx = x;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch)
            .unwrap_or(0)
            .max(1) as u16;
        if cx + w > right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
        if w == 2
            && let Some(cell) = buf.cell_mut((cx + 1, y))
        {
            cell.set_char(' ');
            cell.set_style(style);
        }
        cx += w;
    }
}
