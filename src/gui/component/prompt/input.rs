use std::ops::Range;

use gpui::{
    Bounds, ClipboardItem, Context, EntityInputHandler, KeyDownEvent, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent, UTF16Selection,
    Window,
};
use unicode_segmentation::UnicodeSegmentation;

use super::{Prompt, layout::LINE_HEIGHT};

impl Prompt {
    pub(super) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = &event.keystroke;
        let modifiers = key.modifiers;
        if modifiers == Modifiers::none()
            && matches!(key.key.as_str(), "up" | "down")
            && self.marked.is_none()
            && self.model_draft.is_some()
        {
            self.model_picker.navigate(&self.text, key.key == "down");
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if modifiers == Modifiers::none()
            && matches!(key.key.as_str(), "up" | "down")
            && self.marked.is_none()
            && self.menu.move_selection(&self.text, key.key == "down")
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if key.key == "enter" && modifiers == Modifiers::none() && self.marked.is_none() {
            if !event.is_held {
                self.submit(window, cx);
            }
            cx.stop_propagation();
            return;
        }

        let selection = self.selection();

        if modifiers == Modifiers::secondary_key() {
            match key.key.as_str() {
                "a" => {
                    self.anchor = 0;
                    self.caret = self.text.len();
                }
                "home" | "end" => {
                    self.move_caret(
                        if key.key == "home" {
                            0
                        } else {
                            self.text.len()
                        },
                        false,
                    );
                }
                "c" | "x" => {
                    if !selection.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.text[selection.clone()].into(),
                        ));
                        if key.key == "x" {
                            self.replace(selection, "");
                        }
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.replace(self.replacement_range(None), &text);
                    }
                }
                _ => return,
            }
        } else if !modifiers.control && !modifiers.platform && !modifiers.alt {
            match key.key.as_str() {
                "enter"
                    if modifiers.shift && self.marked.is_none() && self.model_draft.is_none() =>
                {
                    self.replace(selection, "\n");
                }
                "backspace" | "delete" => {
                    let range = if !selection.is_empty() {
                        selection
                    } else if key.key == "backspace" {
                        self.previous_boundary()..self.caret
                    } else {
                        self.caret..self.next_boundary()
                    };
                    self.replace(range, "");
                }
                "left" | "right" | "up" | "down" | "home" | "end" => {
                    let next = match key.key.as_str() {
                        "home" | "end" => self.current_layout().map_or_else(
                            || {
                                if key.key == "home" {
                                    0
                                } else {
                                    self.text.len()
                                }
                            },
                            |layout| layout.line_edge(self.caret, key.key == "end"),
                        ),
                        "up" | "down" => self.current_layout().map_or(self.caret, |layout| {
                            layout.move_vertical(self.caret, key.key == "down")
                        }),
                        "left" if modifiers.shift || selection.is_empty() => {
                            self.previous_boundary()
                        }
                        "right" if modifiers.shift || selection.is_empty() => self.next_boundary(),
                        "left" => selection.start,
                        _ => selection.end,
                    };
                    self.move_caret(next, modifiers.shift);
                }
                _ => return,
            }
        } else {
            return;
        }

        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(layout) = &mut self.layout {
            let scroll = (layout.scroll - event.delta.pixel_delta(LINE_HEIGHT).y)
                .clamp(gpui::px(0.), layout.max_scroll());
            if scroll != layout.scroll {
                layout.scroll = scroll;
                cx.stop_propagation();
                cx.notify();
            }
        }
    }

    pub(super) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus(window);
        self.selecting = true;
        if let Some(layout) = self.current_layout() {
            self.move_caret(layout.index_at(event.position), event.modifiers.shift);
            cx.notify();
        }
    }

    pub(super) fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selecting
            && event.pressed_button == Some(MouseButton::Left)
            && self.focus.is_focused(window)
            && let Some(layout) = self.current_layout()
        {
            self.move_caret(layout.index_at(event.position), true);
            cx.notify();
        }
    }

    fn move_caret(&mut self, offset: usize, extend_selection: bool) {
        self.caret = offset;
        if !extend_selection {
            self.anchor = offset;
        }
    }

    fn previous_boundary(&self) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .find(|(index, _)| *index < self.caret)
            .map_or(0, |(index, _)| index)
    }

    fn next_boundary(&self) -> usize {
        self.text
            .grapheme_indices(true)
            .find(|(index, _)| *index > self.caret)
            .map_or(self.text.len(), |(index, _)| index)
    }

    fn replacement_range(&self, range: Option<Range<usize>>) -> Range<usize> {
        range
            .map(|range| utf16_to_bytes(&self.text, range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection())
    }

    fn replace(&mut self, range: Range<usize>, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = if self.model_draft.is_some() {
            text.replace('\n', " ")
        } else {
            text
        };
        let end = range.start + text.len();
        self.text.replace_range(range, &text);
        self.menu.input_changed();
        if self.model_draft.is_some() {
            self.model_picker.select(0);
        }
        self.marked = None;
        self.move_caret(end, false);
    }
}

// GPUI's platform input handler uses UTF-16, while Rust strings and shaped
// text use byte offsets.
fn utf16_to_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    let to_byte = |offset| {
        let mut units = 0;
        for (index, character) in text.char_indices() {
            if units >= offset {
                return index;
            }
            units += character.len_utf16();
        }
        text.len()
    };
    to_byte(range.start)..to_byte(range.end)
}

fn bytes_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    text[..range.start].encode_utf16().count()..text[..range.end].encode_utf16().count()
}

impl EntityInputHandler for Prompt {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = utf16_to_bytes(&self.text, range);
        *adjusted = Some(bytes_to_utf16(&self.text, range.clone()));
        Some(self.text[range].into())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: bytes_to_utf16(&self.text, self.selection()),
            reversed: self.caret < self.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .clone()
            .map(|range| bytes_to_utf16(&self.text, range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace(self.replacement_range(range), text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.replacement_range(range);
        let start = range.start;
        self.replace(range, text);
        if !text.is_empty() {
            let marked = start..self.caret;
            if let Some(selected) = selected {
                let selection = utf16_to_bytes(&self.text[marked.clone()], selected);
                self.anchor = start + selection.start;
                self.caret = start + selection.end;
            }
            self.marked = Some(marked);
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(
            self.current_layout()?
                .range_bounds(utf16_to_bytes(&self.text, range)),
        )
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.current_layout()?;
        layout
            .bounds
            .contains(&point)
            .then(|| self.text[..layout.index_at(point)].encode_utf16().count())
    }
}
