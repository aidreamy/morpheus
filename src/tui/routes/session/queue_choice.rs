use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use morpheus_tui::core::types::MouseEvent;

use super::super::super::theme::Theme;
use crate::component::wall;
use crate::theme::rgba_color;
use crate::util::draw::draw_text_line;

/// Which pending queue a message typed during a running agent loop joins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueTarget {
    /// The message enters the next LLM request of the CURRENT loop.
    NextRequest,
    /// The message waits for the current loop to end and starts a new one.
    NextLoop,
}

/// The rendered row of a [`QueueTarget`] in the dialog.
const fn option_label(target: QueueTarget) -> &'static str {
    match target {
        QueueTarget::NextRequest => "Next request",
        QueueTarget::NextLoop => "Next agent loop",
    }
}

/// The muted explanation next to [`option_label`].
const fn option_description(target: QueueTarget) -> &'static str {
    match target {
        QueueTarget::NextRequest => "enters the next request of the current loop",
        QueueTarget::NextLoop => "starts a new loop when the current one ends",
    }
}

/// The full row set: every target with a LIVE loop takes both queues - the
/// main agent's session, a morpheus pane's harness loop and an ACP agent's turn
/// (its next request goes out at the prompt boundary, the moment the current
/// prompt completes). [`LOOP_ONLY`] is what a target with no live loop gets
/// (a pane still replaying its history or being compacted): there is no next
/// request to be part of, so the control must not be offered.
const BOTH: [QueueTarget; 2] = [QueueTarget::NextRequest, QueueTarget::NextLoop];
const LOOP_ONLY: [QueueTarget; 1] = [QueueTarget::NextLoop];

/// Inline dialog that asks the user which queue a message typed while the
/// agent loop is running should join. Rendered exactly like the question
/// dialog (left border, panel background, footer hints) so the two share a
/// visual language.
///
/// ONE dialog serves both targets: the MAIN agent's session queues, and -
/// through [`Self::open_for_pane`] - the queues of a single `/agents` pane
/// that is busy. Both targets offer BOTH queues on the same terms (see
/// [`BOTH`]); the pane flavor drops the "next request" row only when that
/// pane has no live loop at all, so the dialog can never name a delivery it
/// could not honor.
///
/// The typed message stays in the prompt input until confirmed; the dialog
/// only carries a one-line preview of it. `Esc` dismisses without losing the
/// message.
pub struct QueueChoiceDialog {
    pub visible: bool,
    /// Index into [`Self::rows`] (0 = the first offered queue).
    pub selected: usize,
    /// Set when the user presses Enter on the selected option. The App reads
    /// it, consumes the message via `PromptView::send_message`, then calls
    /// [`Self::hide`].
    pub submitted: bool,
    /// The `/agents` pane this choice queues for, by stable pane id. `None`
    /// = the MAIN agent's session queues (the original, and only, flow).
    pane: Option<u64>,
    /// Whether the target has a LIVE loop, which is what puts the "next
    /// request" row on offer (see [`BOTH`] / [`LOOP_ONLY`]).
    live_loop: bool,
    /// One-line preview of the message being queued (the prompt keeps the
    /// authoritative text until confirmation).
    preview: String,
}

impl QueueChoiceDialog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            visible: false,
            selected: 0,
            submitted: false,
            pane: None,
            live_loop: true,
            preview: String::new(),
        }
    }

    /// Open the dialog for the given message preview - the MAIN agent's
    /// session queues, both targets on offer.
    pub fn open(&mut self, preview: impl Into<String>) {
        self.preview = preview.into();
        self.pane = None;
        self.live_loop = true;
        self.selected = 0;
        self.submitted = false;
        self.visible = true;
    }

    /// Open the dialog for ONE `/agents` pane's queues - the same two targets
    /// the main agent's session offers, for BOTH pane kinds.
    ///
    /// `live_loop` is `false` only when that pane has no loop to be the next
    /// request OF (its bound history is still replaying, or its session is
    /// being compacted): then only the next-loop row is offered (and selected
    /// by default). A RUNNING pane always offers both, morpheus or ACP - what
    /// differs is only WHEN the next request can be delivered (mid-turn for a
    /// morpheus harness, at the prompt boundary for an ACP agent), which is the
    /// turn's business, not the user's choice.
    pub fn open_for_pane(&mut self, preview: impl Into<String>, pane: u64, live_loop: bool) {
        self.open(preview);
        self.pane = Some(pane);
        self.live_loop = live_loop;
        // The single-row case IS the loop queue; `selected` stays 0 and
        // indexes into the offered rows, so no extra adjustment is needed.
    }

    /// Close the dialog (after submission or dismissal).
    pub fn hide(&mut self) {
        self.visible = false;
        self.submitted = false;
        self.pane = None;
        self.live_loop = true;
        self.selected = 0;
    }

    /// The pane this choice queues for (only meaningful after submit).
    /// `None` = the main agent's session queues.
    #[must_use]
    pub const fn pane(&self) -> Option<u64> {
        self.pane
    }

    /// The rows actually offered, in rendered order.
    #[must_use]
    fn rows(&self) -> &'static [QueueTarget] {
        if self.live_loop { &BOTH } else { &LOOP_ONLY }
    }

    /// The queue selected by the user (only meaningful after submit).
    #[must_use]
    pub fn choice(&self) -> QueueTarget {
        let rows = self.rows();
        rows[self.selected.min(rows.len() - 1)]
    }

    /// Handle a key event. Returns true if consumed.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> bool {
        if !self.visible {
            return false;
        }
        let code = key.code;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Navigation is bounded by the OFFERED rows: a single-row dialog
        // must not select a queue it does not show.
        let last = self.rows().len() - 1;
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.min(last).saturating_sub(1);
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(last);
                true
            }
            // Number keys select directly (like the question dialog).
            KeyCode::Char('1') => {
                self.selected = 0;
                true
            }
            KeyCode::Char('2') => {
                self.selected = 1.min(last);
                true
            }
            KeyCode::Enter => {
                self.submitted = true;
                true
            }
            KeyCode::Esc => {
                self.visible = false;
                true
            }
            KeyCode::Char(_) if ctrl => true,
            _ => false,
        }
    }

    /// Required height in rows (0 when hidden).
    #[must_use]
    pub fn required_height(&self, _max_width: u16) -> u16 {
        if !self.visible {
            return 0;
        }
        // padding_vertical (5) + question row + preview row + one row per
        // offered queue + footer (1).
        7 + self.rows().len() as u16
    }

    /// Handle a mouse click on the dialog. `area` is the area passed to render.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect) -> bool {
        if !self.visible {
            return false;
        }
        let x = mouse.x;
        let y_click = mouse.y;
        let height = self.required_height(area.width).min(area.height);
        if x < area.x || x >= area.x + area.width || y_click < area.y || y_click >= area.y + height
        {
            return false;
        }

        let inner_x = area.x + 3;
        // Layout mirrors `render`: question at area.y+1, preview at +2,
        // options at +4 and +5, footer at area.y + height - 2.
        let option_y = area.y + 4;
        for (i, _) in self.rows().iter().enumerate() {
            if y_click == option_y + i as u16 {
                self.selected = i;
                self.submitted = true;
                return true;
            }
        }
        // Footer hints: "enter confirm" spans [inner_x, inner_x+13) and
        // "esc dismiss" spans [inner_x+13, inner_x+21) - must mirror `render`.
        let footer_y = area.y + height.saturating_sub(2);
        if y_click == footer_y {
            if x >= inner_x && x < inner_x + 13 {
                self.submitted = true;
                return true;
            }
            if x >= inner_x + 13 && x < inner_x + 21 {
                self.visible = false;
                return true;
            }
        }
        true
    }

    /// Render the dialog inline inside the given area.
    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }
        let height = self.required_height(area.width).min(area.height);
        let inner_area = Rect::new(area.x, area.y, area.width, height);
        let footer_y = inner_area.bottom().saturating_sub(2);

        wall::render(buf, inner_area, theme.accent, theme.background_panel);

        let pad = 3u16;
        let inner_x = area.x + pad;
        let inner_w = area.width.saturating_sub(pad + 2);
        let bg = rgba_color(theme.background_panel);

        // Question row.
        draw_text_line(
            buf,
            "When to send this message?",
            inner_x,
            area.y + 1,
            inner_w,
            Style::default().fg(rgba_color(theme.text)).bg(bg),
        );

        // Message preview row (muted, truncated with an ellipsis).
        let preview = Self::truncate(&self.preview, inner_w);
        draw_text_line(
            buf,
            &preview,
            inner_x,
            area.y + 2,
            inner_w,
            Style::default().fg(rgba_color(theme.text_muted)).bg(bg),
        );

        // Option rows.
        let option_y = area.y + 4;
        for (i, target) in self.rows().iter().enumerate() {
            let ry = option_y + i as u16;
            if ry >= footer_y {
                break;
            }
            let is_selected = i == self.selected;
            // Marker glyph matching the permission dialog's selected option
            // (the "asterisk" marker), with a blank slot for unselected rows.
            let indicator = if is_selected { "\u{1F7B4} " } else { "  " };
            let indicator_color = if is_selected {
                theme.accent
            } else {
                theme.text_muted
            };
            // Highlight the active row across the full inner width.
            if is_selected {
                let bg_color = rgba_color(theme.background_element);
                for cx in inner_x..inner_x + inner_w {
                    if let Some(cell) = buf.cell_mut((cx, ry)) {
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(bg_color));
                    }
                }
            }
            draw_text_line(
                buf,
                indicator,
                inner_x,
                ry,
                inner_w,
                Style::default()
                    .fg(rgba_color(indicator_color))
                    .bg(if is_selected {
                        rgba_color(theme.background_element)
                    } else {
                        bg
                    }),
            );
            let text_fg = if is_selected {
                theme.secondary
            } else {
                theme.text
            };
            let opt_w = inner_w.saturating_sub(3);
            let combined = Self::truncate(
                &format!(
                    "{} · {}",
                    option_label(*target),
                    option_description(*target)
                ),
                opt_w,
            );
            draw_text_line(
                buf,
                &combined,
                inner_x + 3,
                ry,
                opt_w,
                Style::default().fg(rgba_color(text_fg)).bg(if is_selected {
                    rgba_color(theme.background_element)
                } else {
                    bg
                }),
            );
        }

        // Footer (keyboard hints).
        draw_text_line(
            buf,
            "enter",
            inner_x,
            footer_y,
            inner_w,
            Style::default().fg(rgba_color(theme.text)).bg(bg),
        );
        draw_text_line(
            buf,
            " confirm",
            inner_x + 5,
            footer_y,
            inner_w,
            Style::default().fg(rgba_color(theme.text_muted)).bg(bg),
        );
        draw_text_line(
            buf,
            " esc",
            inner_x + 13,
            footer_y,
            inner_w,
            Style::default().fg(rgba_color(theme.text_muted)).bg(bg),
        );
        draw_text_line(
            buf,
            " dismiss",
            inner_x + 17,
            footer_y,
            inner_w,
            Style::default().fg(rgba_color(theme.text_muted)).bg(bg),
        );
    }

    /// Truncate `text` to at most `width` display columns, appending `…` when
    /// cut (leaves room for the ellipsis).
    fn truncate(text: &str, width: u16) -> String {
        let width = usize::from(width).max(1);
        let mut out: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(width)
            .collect();
        if text.chars().filter(|c| !c.is_control()).count() > width {
            out.pop();
            out.push('…');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{QueueChoiceDialog, QueueTarget};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn open_resets_selection_and_shows_preview() {
        let mut d = QueueChoiceDialog::new();
        assert!(!d.visible);
        assert_eq!(d.required_height(60), 0);
        d.open("fix the test");
        assert!(d.visible);
        assert_eq!(d.selected, 0);
        assert_eq!(d.choice(), QueueTarget::NextRequest);
    }

    #[test]
    fn navigation_toggles_between_the_two_queues() {
        let mut d = QueueChoiceDialog::new();
        d.open("msg");
        d.handle_key_event(key(KeyCode::Down));
        assert_eq!(d.choice(), QueueTarget::NextLoop);
        d.handle_key_event(key(KeyCode::Char('k')));
        assert_eq!(d.choice(), QueueTarget::NextRequest);
        d.handle_key_event(key(KeyCode::Char('2')));
        assert_eq!(d.choice(), QueueTarget::NextLoop);
        d.handle_key_event(key(KeyCode::Char('1')));
        assert_eq!(d.choice(), QueueTarget::NextRequest);
    }

    #[test]
    fn enter_submits_and_esc_dismisses() {
        let mut d = QueueChoiceDialog::new();
        d.open("msg");
        assert!(!d.submitted);
        d.handle_key_event(key(KeyCode::Enter));
        assert!(d.submitted);
        assert!(d.visible);

        d.open("msg");
        d.handle_key_event(key(KeyCode::Esc));
        assert!(!d.visible);
        assert!(!d.submitted);
    }

    #[test]
    fn hidden_dialog_consumes_nothing_and_has_zero_height() {
        let mut d = QueueChoiceDialog::new();
        assert!(!d.handle_key_event(key(KeyCode::Enter)));
        assert_eq!(d.required_height(80), 0);
    }

    #[test]
    fn a_pane_without_a_live_loop_offers_only_the_next_loop() {
        let mut d = QueueChoiceDialog::new();
        d.open_for_pane("msg", 7, false);
        assert_eq!(d.pane(), Some(7));
        assert_eq!(
            d.choice(),
            QueueTarget::NextLoop,
            "the steer row is not offered, so the default is the loop"
        );
        // Navigation cannot select a row the dialog does not show.
        d.handle_key_event(key(KeyCode::Up));
        d.handle_key_event(key(KeyCode::Char('1')));
        assert_eq!(d.choice(), QueueTarget::NextLoop);
        // One row instead of two: the box is a row shorter.
        assert_eq!(d.required_height(60), 8);
    }

    #[test]
    fn a_pane_with_a_live_loop_offers_both_queues_and_hide_clears_the_target() {
        let mut d = QueueChoiceDialog::new();
        d.open_for_pane("msg", 3, true);
        assert_eq!(d.pane(), Some(3));
        assert_eq!(d.choice(), QueueTarget::NextRequest);
        d.handle_key_event(key(KeyCode::Char('2')));
        assert_eq!(d.choice(), QueueTarget::NextLoop);
        assert_eq!(d.required_height(60), 9);
        // Hiding drops the target: the NEXT open (the main agent's) must not
        // inherit the pane.
        d.hide();
        assert_eq!(d.pane(), None);
        d.open("main");
        assert_eq!(d.pane(), None);
        assert_eq!(d.choice(), QueueTarget::NextRequest);
    }

    #[test]
    fn truncate_appends_ellipsis() {
        assert_eq!(QueueChoiceDialog::truncate("hello", 40), "hello");
        assert_eq!(QueueChoiceDialog::truncate("hello world", 8), "hello w…");
        // Control characters are dropped (ratatui buffer safety).
        assert_eq!(QueueChoiceDialog::truncate("a\u{1b}[31mb", 40), "a[31mb");
    }
}
