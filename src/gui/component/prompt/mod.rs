mod input;
mod send_button;

use std::ops::Range;

use gpui::{
    Bounds, BoxShadow, CursorStyle, ElementInputHandler, EventEmitter, FocusHandle, MouseButton,
    Pixels, Point, ShapedLine, TextRun, Window, canvas, div, fill, point, prelude::*, px, rgb,
    rgba, size,
};

use crate::theme;

pub(crate) struct Prompt {
    focus: FocusHandle,
    text: String,
    anchor: usize,
    caret: usize,
    marked: Option<Range<usize>>,
    layout: Option<Line>,
    selecting: bool,
}

pub(crate) struct Submitted(pub(crate) String);

impl EventEmitter<Submitted> for Prompt {}

impl Prompt {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            text: String::new(),
            anchor: 0,
            caret: 0,
            marked: None,
            layout: None,
            selecting: false,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window) {
        self.focus.focus(window);
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.text.trim().is_empty() || self.marked.is_some() {
            return;
        }

        let text = std::mem::take(&mut self.text);
        self.anchor = 0;
        self.caret = 0;
        self.layout = None;
        self.selecting = false;
        cx.emit(Submitted(text));
        cx.notify();
    }

    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    fn current_layout(&self) -> Option<&Line> {
        self.layout
            .as_ref()
            .filter(|line| line.shaped.text.as_ref() == self.text)
    }

    fn shape(&self, bounds: Bounds<Pixels>, window: &Window) -> Line {
        let style = window.text_style();
        let run = TextRun {
            len: self.text.len(),
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window.text_system().shape_line(
            self.text.clone().into(),
            style.font_size.to_pixels(window.rem_size()),
            &[run],
            None,
        );
        let caret_x = shaped.x_for_index(self.caret);
        let visible_width = (bounds.size.width - px(2.)).max(px(0.));
        let previous_scroll = self.layout.as_ref().map_or(px(0.), |line| line.scroll);
        let scroll = previous_scroll
            .clamp((caret_x - visible_width).max(px(0.)), caret_x)
            .min((shaped.width - visible_width).max(px(0.)));
        Line {
            shaped,
            bounds,
            scroll,
        }
    }
}

struct Line {
    shaped: ShapedLine,
    bounds: Bounds<Pixels>,
    scroll: Pixels,
}

impl Line {
    fn origin(&self) -> Point<Pixels> {
        self.bounds.origin - point(self.scroll, px(0.))
    }

    fn range_bounds(&self, range: Range<usize>) -> Bounds<Pixels> {
        let start = self.origin().x + self.shaped.x_for_index(range.start);
        let end = self.origin().x + self.shaped.x_for_index(range.end);
        Bounds::from_corners(
            point(start.min(end), self.bounds.top()),
            point(start.max(end), self.bounds.bottom()),
        )
    }

    fn index_at(&self, position: Point<Pixels>) -> usize {
        self.shaped
            .closest_index_for_x(position.x - self.origin().x)
    }
}

impl Render for Prompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prompt = cx.entity();
        let prompt_for_layout = prompt.clone();
        let focused = self.focus.is_focused(window);
        let line = canvas(
            move |bounds, window, cx| prompt_for_layout.read(cx).shape(bounds, window),
            move |bounds, line, window, cx| {
                let input = prompt.read(cx);
                let selection = input.selection();
                window.handle_input(
                    &input.focus,
                    ElementInputHandler::new(bounds, prompt.clone()),
                    cx,
                );
                if focused && !selection.is_empty() {
                    window.paint_quad(fill(
                        line.range_bounds(selection.clone()),
                        rgb(theme::FOCUS),
                    ));
                }
                if let Some(marked) = &input.marked {
                    let marked = line.range_bounds(marked.clone());
                    window.paint_quad(fill(
                        Bounds::new(
                            point(marked.left(), marked.bottom() - px(2.)),
                            size(marked.size.width, px(1.)),
                        ),
                        rgb(theme::MUTED),
                    ));
                }
                let _ = line
                    .shaped
                    .paint(line.origin(), bounds.size.height, window, cx);
                if focused && selection.is_empty() {
                    let caret = line.range_bounds(selection);
                    window.paint_quad(fill(
                        Bounds::new(caret.origin, size(px(1.), caret.size.height)),
                        rgb(theme::TEXT),
                    ));
                }
                prompt.update(cx, |prompt, _| prompt.layout = Some(line));
            },
        )
        .w_full()
        .h(px(22.));

        let input = div()
            .id("prompt")
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .flex()
            .items_center()
            .gap(px(12.))
            .w_full()
            .h(px(80.))
            .px(px(16.))
            .rounded(px(16.))
            .border_1()
            .border_color(rgba(if focused { 0xffffff22 } else { 0xffffff16 }))
            .bg(rgb(theme::SURFACE))
            .shadow(vec![BoxShadow {
                color: rgba(0x00000033).into(),
                offset: point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
            }])
            .text_size(px(15.))
            .text_color(rgb(theme::TEXT))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .child(div().flex_1().min_w_0().overflow_hidden().child(line))
            .when(!self.text.trim().is_empty(), |input| {
                input.child(send_button::render(
                    cx.listener(|prompt, _, _, cx| prompt.submit(cx)),
                ))
            });

        div()
            .relative()
            .w_full()
            .pt(px(28.))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(16.))
                    .right(px(16.))
                    .h(px(52.))
                    .rounded(px(16.))
                    .border_1()
                    .border_color(rgb(theme::BORDER))
                    .bg(rgb(theme::CHROME)),
            )
            .child(input)
    }
}
