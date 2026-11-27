//! The pending-queue strip: the band of queued messages that rises above the
//! prompt (the chat's) or above a pane's bottom edge (an `/agents` pane's).
//!
//! One renderer serves BOTH surfaces because they carry the same thing - the
//! two queues of a [`PendingQueues`] - and the user must read them the same
//! way wherever they are: a lighter wash of the surface's hue per message,
//! each band ending in a full-strength color cap labeled `NEXT` (the message
//! joins the very next request of the running loop) or `CLOSURE` (it waits
//! for that loop to end, then starts a fresh one).
//!
//! The only thing the caller supplies that differs is `cap_color`: the
//! chat passes the current harness mode's color (the same hue its prompt's
//! mode indicator paints), a pane passes its agent's brand color.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::state::PendingQueues;
use crate::theme::{Theme, contrast_color, rgba_color};
use morpheus_tui::core::lib::rgba::RGBA;
use morpheus_tui::core::lib::unicode_util::graphemes_with_width;

/// One visual row of the pending-queue strip. Every queued message owns
/// its own internal padding: a `Pad` row directly above its first text
/// line and directly below its last one, painted as part of the same
/// queue-colored band (same chrome, no text). Two queued messages are
/// therefore always separated by their respective padding rows, and the
/// text never touches whatever sits below the strip (the prompt box, a
/// pane's edge) or a neighboring message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingQueueRow {
    /// Internal padding owned by one message: painted as part of that
    /// message's band (same chrome, no text). Carries
    /// `(queue_index, message_index)` so hover/click on the pad resolve
    /// to the message it belongs to.
    Pad(usize, usize),
    /// A text line: `(queue_index, message_index, line)` - exactly ONE per
    /// message, already truncated to the strip's width.
    Text(usize, usize, String),
}

impl PendingQueueRow {
    /// `(queue_index, message_index)` the row belongs to (drives the band
    /// color and the hit-testing target).
    #[must_use]
    pub fn owner(&self) -> (usize, usize) {
        match self {
            Self::Pad(queue_idx, msg_idx) | Self::Text(queue_idx, msg_idx, _) => {
                (*queue_idx, *msg_idx)
            }
        }
    }
}

/// Width of the color end cap: ~20% of the band width, never narrower than
/// the longest label (`CLOSURE`) plus padding. `None` when the band is too
/// narrow to host a cap and still leave readable text. Shared by the layout
/// (which reserves the cap's columns when truncating) and the painter so
/// text never runs under the cap.
#[must_use]
pub fn cap_width(width: u16) -> Option<u16> {
    const MIN_CAP_W: u16 = 9; // "CLOSURE" (7) + side padding
    if width < 20 {
        return None;
    }
    Some(
        (width * 20 / 100)
            .max(MIN_CAP_W)
            .min(width.saturating_sub(10)),
    )
}

/// The layout of a strip of `queues` at `width` columns: one layout entry
/// per drawn row, so the renderer, the height reservation and the mouse
/// hit-testing all agree.
///
/// Each queued message yields exactly ONE `Text` row - truncated to the
/// columns available outside the end cap, never word-wrapped (the full text
/// still goes to the model; only the strip's preview is cut) - bracketed by
/// its own `Pad` rows, in `next loop` then `next request` order.
#[must_use]
pub fn layout(queues: &PendingQueues, width: u16) -> Vec<PendingQueueRow> {
    let text_w = match cap_width(width) {
        // │ + 2 pad + text + gap + cap + │
        Some(cap_w) => width.saturating_sub(cap_w + 5),
        None => width.saturating_sub(6), // │ + 2 pad left, 2 pad right + │
    } as usize;
    let one_line = |text: &str| -> String {
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        let mut line = String::new();
        let mut used = 0usize;
        for (grapheme, w) in graphemes_with_width(&clean) {
            let w = usize::from(w);
            if used + w > text_w.max(1) {
                break; // visual cut: no ellipsis, the rest simply vanishes
            }
            line.push_str(grapheme);
            used += w;
        }
        line
    };
    let mut rows = Vec::new();
    for (qi, queue) in [&queues.next_loop, &queues.next_request]
        .into_iter()
        .enumerate()
    {
        for (mi, text) in queue.iter().enumerate() {
            // Each message carries its own top and bottom padding, so
            // neighboring messages are always separated and the strip's
            // outer edges get one pad row too.
            rows.push(PendingQueueRow::Pad(qi, mi));
            rows.push(PendingQueueRow::Text(qi, mi, one_line(text)));
            rows.push(PendingQueueRow::Pad(qi, mi));
        }
    }
    rows
}

/// How many rows [`render`] will paint for `queues` at `width` - the height
/// a caller reserves before laying out the rest of its surface.
#[must_use]
pub fn height(queues: &PendingQueues, width: u16) -> u16 {
    u16::try_from(layout(queues, width).len()).unwrap_or(u16::MAX)
}

/// Band background for the ~80% of each message's band outside the color
/// cap: the cap color lightened toward white (~15% blend). The full color
/// stays in the cap, so the cap reads as a slightly deeper chip on a lighter
/// band of the same hue. RGB channels only - alpha comes back opaque,
/// matching every other band fill.
fn lightened_band(color: RGBA) -> RGBA {
    const T: f32 = 0.15; // share of white mixed in
    let (r, g, b, _) = color.to_ints();
    let up = |c: u8| -> u8 { (f32::from(c) * (1.0 - T) + 255.0 * T).round() as u8 };
    RGBA::from_ints(up(r), up(g), up(b), 255)
}

/// The queue label carried by a message's end cap: queue 1 ("next request",
/// injected into the very next request of the running loop) reads `NEXT`;
/// queue 0 ("next agent loop", sent only when a fresh loop starts) reads
/// `CLOSURE` - it closes out the current loop's run.
fn cap_label(queue_idx: usize) -> &'static str {
    if queue_idx == 0 { "CLOSURE" } else { "NEXT" }
}

/// Paint the strip of `queues` into `area` (top row first; rows past the
/// area's bottom are clipped).
///
/// `hover` is the `(queue_index, message_index)` under the pointer, when the
/// caller routes a pointer at all: that message's whole band lights up.
/// `cap_color` fills each band's end cap and carries the `NEXT`/`CLOSURE`
/// label - see the module docs for what each surface passes.
pub fn render(
    buf: &mut Buffer,
    area: Rect,
    queues: &PendingQueues,
    hover: Option<(usize, usize)>,
    theme: &Theme,
    cap_color: RGBA,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    render_rows(
        buf,
        area,
        &layout(queues, area.width),
        hover,
        theme,
        cap_color,
    );
}

/// Paint an ALREADY COMPUTED strip layout (see [`layout`]) - for callers
/// that laid the strip out once for this frame and must not re-truncate it
/// in the painter (the chat's geometry, height computation and hit-testing
/// all share the same rows).
pub fn render_rows(
    buf: &mut Buffer,
    area: Rect,
    rows: &[PendingQueueRow],
    hover: Option<(usize, usize)>,
    theme: &Theme,
    cap_color: RGBA,
) {
    if rows.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    // The hovered row swaps its queue color for pure WHITE so the focus
    // clearly stands out against every theme surface (and against the
    // prompt box, which shares the element/panel colors). `contrast_color`
    // flips the row text to black for readability.
    let hover_bg = RGBA::from_hex("#FFFFFF");
    let queue_bg = lightened_band(cap_color);
    // Band extents: contiguous layout rows sharing an owner form one
    // message's band (its pads + text). The cap's label is written on the
    // middle row of each band so it sits visually centered.
    let mut bands: Vec<((usize, usize), usize, usize)> = Vec::new();
    for (i, entry) in rows.iter().enumerate() {
        let owner = entry.owner();
        match bands.last_mut() {
            Some((last_owner, _, len)) if *last_owner == owner => *len += 1,
            _ => bands.push((owner, i, 1)),
        }
    }
    let mut label_at = vec![false; rows.len()];
    for (_, start, len) in &bands {
        label_at[start + len / 2] = true;
    }
    let cap_w = cap_width(area.width);
    for (y, (i, entry)) in (area.y..).zip(rows.iter().enumerate()) {
        if y >= area.bottom() {
            break;
        }
        let (queue_idx, msg_idx) = entry.owner();
        let bg = if hover == Some((queue_idx, msg_idx)) {
            hover_bg
        } else {
            queue_bg
        };
        let line = match entry {
            PendingQueueRow::Text(_, _, line) => line.as_str(),
            // Padding: same band chrome, no message text.
            PendingQueueRow::Pad(..) => "",
        };
        draw_row(
            buf,
            line,
            area.x,
            y,
            area.width,
            bg,
            theme.background_panel,
            cap_color,
        );
        // Color end cap (~20% of the band, right-aligned before the right
        // `│`): fills every row of the message's band so it reads as one
        // solid block, and carries the queue label bold on the band's
        // middle row. The cap keeps its color even while the band is
        // hovered - it is the identity chip, not part of the highlight.
        if let Some(cap_w) = cap_w {
            let cap_right = area.x + area.width - 1;
            let cap_x = cap_right - cap_w;
            let cap_fg = contrast_color(cap_color);
            let fill_style = Style::default().bg(rgba_color(cap_color));
            for cx in cap_x..cap_right {
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    cell.set_char(' ');
                    cell.set_style(fill_style);
                }
            }
            if label_at[i] {
                let label = cap_label(queue_idx);
                let label_style = fill_style
                    .fg(cap_fg)
                    .add_modifier(ratatui::style::Modifier::BOLD);
                let label_w = label.chars().count() as u16;
                let label_x = cap_x + cap_w.saturating_sub(label_w) / 2;
                for (lx, ch) in (label_x..).zip(label.chars()) {
                    if lx >= cap_right {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((lx, y)) {
                        cell.set_char(ch);
                        cell.set_style(label_style);
                    }
                }
            }
        }
    }
}

/// Draw one pending-message line: the `│` left and right borders in the cap
/// color (the same hue the band's chip carries, so the strip's side rails
/// read as the same element) on the neutral panel background, the band color
/// as the background of the rest of the row (starting right after the border,
/// at `x + 1`, so it never covers the `│` glyph), and the (already
/// truncated) message text starting 3 columns in - like a normal user
/// message, so queued rows stay visually consistent with the chat.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    buf: &mut Buffer,
    text: &str,
    x: u16,
    y: u16,
    width: u16,
    bg: RGBA,
    panel: RGBA,
    border: RGBA,
) {
    if width < 6 {
        return;
    }
    let fg = contrast_color(bg);
    let bg_color = rgba_color(bg);
    // Side rails (│) in the cap color on the neutral panel background.
    let rail_style = Style::default()
        .fg(rgba_color(border))
        .bg(rgba_color(panel));
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_char('│');
        cell.set_style(rail_style);
    }
    // Matching right border (│), mirroring the prompt box's symmetric chrome.
    if let Some(cell) = buf.cell_mut((x + width - 1, y)) {
        cell.set_char('│');
        cell.set_style(rail_style);
    }
    // Band-colored background for the rest of the row - it stops one column
    // short of the right edge so the right `│` sits on the panel background.
    let band_style = Style::default().bg(bg_color);
    for cx in x + 1..x + width - 1 {
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(' ');
            cell.set_style(band_style);
        }
    }
    // Message text starts 3 columns in (│ + 2 pad) and stops 3 short of the
    // right edge (2 pad + │); `text` is pre-truncated by [`layout`].
    let text_x = x + 3;
    let text_right = x + width - 3;
    let text_style = Style::default().fg(fg).bg(bg_color);
    let mut cx = text_x;
    for (grapheme, w) in graphemes_with_width(text) {
        if cx + w > text_right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            if grapheme.len() == 1 {
                cell.set_char(grapheme.chars().next().unwrap());
            } else {
                cell.set_symbol(grapheme);
            }
            cell.set_style(text_style);
        }
        if w > 1 {
            for dx in 1..w {
                if let Some(cell) = buf.cell_mut((cx + dx, y)) {
                    cell.set_diff_option(ratatui::buffer::CellDiffOption::Skip);
                }
            }
        }
        cx += w;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queues(loop_msgs: &[&str], request_msgs: &[&str]) -> PendingQueues {
        let mut q = PendingQueues::default();
        q.next_loop
            .extend(loop_msgs.iter().map(|s| (*s).to_string()));
        q.next_request
            .extend(request_msgs.iter().map(|s| (*s).to_string()));
        q
    }

    #[test]
    fn every_message_owns_a_pad_text_pad_band() {
        let q = queues(&["closure"], &["next"]);
        let rows = layout(&q, 40);
        assert_eq!(rows.len(), 6);
        assert_eq!(height(&q, 40), 6);
        // next loop first (queue 0), then next request (queue 1).
        assert_eq!(rows[0], PendingQueueRow::Pad(0, 0));
        assert_eq!(rows[1], PendingQueueRow::Text(0, 0, "closure".into()));
        assert_eq!(rows[2], PendingQueueRow::Pad(0, 0));
        assert_eq!(rows[3], PendingQueueRow::Pad(1, 0));
        assert_eq!(rows[1].owner(), (0, 0));
        assert_eq!(rows[4].owner(), (1, 0));
    }

    #[test]
    fn an_empty_queue_reserves_no_rows() {
        assert!(layout(&PendingQueues::default(), 40).is_empty());
        assert_eq!(height(&PendingQueues::default(), 40), 0);
    }

    #[test]
    fn the_cap_reserves_room_and_a_narrow_band_drops_it() {
        // Wide enough for the cap: the label "CLOSURE" plus padding.
        assert!(cap_width(40).is_some());
        // Too narrow to host a cap and still leave readable text.
        assert_eq!(cap_width(19), None);
        // The layout still yields one row per message without a cap.
        let rows = layout(&queues(&["a long message that must be cut"], &[]), 12);
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[1], PendingQueueRow::Text(0, 0, line) if line.chars().count() <= 6));
    }

    #[test]
    fn control_characters_never_reach_the_strip() {
        let rows = layout(&queues(&["tab\there\nnewline"], &[]), 40);
        assert_eq!(
            rows[1],
            PendingQueueRow::Text(0, 0, "tabherenewline".into())
        );
    }
}
