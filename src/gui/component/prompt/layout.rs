use std::ops::Range;

use gpui::{
    App, AvailableSpace, Bounds, ContentMask, ElementInputHandler, Entity, GlobalElementId,
    InspectorElementId, LayoutId, Pixels, Point, SharedString, Size, Style, TextAlign, TextRun,
    TextStyle, Window, WrappedLine, fill, point, prelude::*, px, relative, rgb, size,
};

use super::Prompt;
use crate::theme;

pub(super) const LINE_HEIGHT: Pixels = px(22.);

pub(super) struct Editor(pub(super) Entity<Prompt>);

fn display_text(prompt: &Prompt) -> SharedString {
    if prompt.masked() {
        // Password fields accept ASCII, so masking preserves byte offsets.
        "*".repeat(prompt.text.len()).into()
    } else {
        prompt.text.clone().into()
    }
}

pub(super) struct Layout {
    pub(super) text: SharedString,
    pub(super) bounds: Bounds<Pixels>,
    pub(super) scroll: Pixels,
    caret: usize,
    height: Pixels,
    lines: Vec<Line>,
}

struct Line {
    shaped: WrappedLine,
    start: usize,
    top: Pixels,
}

impl Layout {
    fn shape(
        text: SharedString,
        bounds: Bounds<Pixels>,
        style: &TextStyle,
        window: &Window,
    ) -> Self {
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let wrap_width = (bounds.size.width - px(2.)).max(px(1.));
        let shaped = window
            .text_system()
            .shape_text(
                text.clone(),
                style.font_size.to_pixels(window.rem_size()),
                &[run],
                Some(wrap_width),
                None,
            )
            .unwrap_or_default();
        let mut start = 0;
        let mut top = px(0.);
        let lines = shaped
            .into_iter()
            .map(|shaped| {
                let line = Line { shaped, start, top };
                start += line.shaped.len() + 1;
                top += line.shaped.size(LINE_HEIGHT).height;
                line
            })
            .collect();
        Self {
            text,
            bounds,
            lines,
            scroll: px(0.),
            caret: 0,
            height: top.max(LINE_HEIGHT),
        }
    }

    fn origin(&self) -> Point<Pixels> {
        self.bounds.origin - point(px(0.), self.scroll)
    }

    pub(super) fn max_scroll(&self) -> Pixels {
        (self.height - self.bounds.size.height).max(px(0.))
    }

    fn position_at(&self, index: usize) -> Point<Pixels> {
        let Some(line) = self.lines.iter().rfind(|line| line.start <= index) else {
            return self.origin();
        };
        let position = line
            .shaped
            .position_for_index(index - line.start, LINE_HEIGHT)
            .unwrap_or_default();
        self.origin() + point(px(0.), line.top) + position
    }

    pub(super) fn index_at(&self, position: Point<Pixels>) -> usize {
        let mut position = position - self.origin();
        position.y = position.y.clamp(px(0.), self.height - px(1.));
        self.lines
            .iter()
            .rfind(|line| line.top <= position.y)
            .map_or(0, |line| {
                let position = position - point(px(0.), line.top);
                line.start
                    + line
                        .shaped
                        .closest_index_for_position(position, LINE_HEIGHT)
                        .unwrap_or_else(|edge| edge)
            })
    }

    pub(super) fn move_vertical(&self, index: usize, down: bool) -> usize {
        let position =
            self.position_at(index) + point(px(0.), if down { LINE_HEIGHT } else { -LINE_HEIGHT });
        self.index_at(position)
    }

    pub(super) fn line_edge(&self, index: usize, end: bool) -> usize {
        let position = self.position_at(index);
        self.index_at(point(
            if end {
                self.bounds.right()
            } else {
                self.bounds.left() - px(1.)
            },
            position.y,
        ))
    }

    fn selection_rows(&self, range: Range<usize>) -> impl Iterator<Item = Bounds<Pixels>> + '_ {
        self.lines.iter().flat_map(move |line| {
            let shaped = &line.shaped;
            let mut row_start = 0;
            let range = range.clone();
            shaped
                .wrap_boundaries()
                .iter()
                .map(|boundary| shaped.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
                .chain([shaped.len()])
                .enumerate()
                .filter_map(move |(row, row_end)| {
                    let row_first = std::mem::replace(&mut row_start, row_end);
                    let start = range.start.saturating_sub(line.start).max(row_first);
                    let end = range.end.saturating_sub(line.start).min(row_end);
                    let has_newline = row == shaped.wrap_boundaries().len()
                        && range.contains(&(line.start + row_end))
                        && line.start + row_end < self.text.len();
                    if start >= end && !has_newline {
                        return None;
                    }
                    let x = |offset| {
                        shaped.unwrapped_layout.x_for_index(offset)
                            - shaped.unwrapped_layout.x_for_index(row_first)
                    };
                    let x_start = x(start.min(row_end));
                    let x_end = x(end);
                    let origin =
                        self.origin() + point(x_start.min(x_end), line.top + LINE_HEIGHT * row);
                    let width = (x_start - x_end).abs() + if has_newline { px(4.) } else { px(0.) };
                    Some(Bounds::new(origin, size(width, LINE_HEIGHT)))
                })
        })
    }

    pub(super) fn range_bounds(&self, range: Range<usize>) -> Bounds<Pixels> {
        self.selection_rows(range.clone())
            .next()
            .unwrap_or_else(|| {
                Bounds::new(self.position_at(range.start), size(px(1.), LINE_HEIGHT))
            })
    }
}

impl IntoElement for Editor {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Editor {
    type RequestLayoutState = TextStyle;
    type PrepaintState = Option<Layout>;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let prompt = self.0.read(cx);
        let text = display_text(prompt);
        let max_height = if prompt.single_line() {
            LINE_HEIGHT
        } else {
            let height = window.viewport_size().height - px(250.);
            height.clamp(LINE_HEIGHT * 2., LINE_HEIGHT * 7.)
        };
        let text_style = window.text_style();
        let measure_style = text_style.clone();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.min_size.height = LINE_HEIGHT.into();
        let id = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.unwrap_or(match available.width {
                AvailableSpace::Definite(width) => width,
                AvailableSpace::MinContent => px(1.),
                AvailableSpace::MaxContent => window.viewport_size().width,
            });
            let bounds = Bounds::new(Point::default(), size(width, px(0.)));
            let layout = Layout::shape(text.clone(), bounds, &measure_style, window);
            let available_height = known.height.unwrap_or(match available.height {
                AvailableSpace::Definite(height) => height,
                AvailableSpace::MinContent | AvailableSpace::MaxContent => max_height,
            });
            Size {
                width,
                height: layout.height.min(max_height).min(available_height),
            }
        });
        (id, text_style)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        style: &mut TextStyle,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let prompt = self.0.read(cx);
        let text = display_text(prompt);
        let mut layout = Layout::shape(text.clone(), bounds, style, window);
        layout.caret = prompt.caret;
        let previous = prompt.layout.as_ref();
        let scroll = previous.map_or(px(0.), |layout| layout.scroll);
        let reveal = previous.is_none_or(|layout| {
            layout.text != text || layout.caret != prompt.caret || layout.bounds.size != bounds.size
        });
        let scroll = if reveal {
            let caret_y = layout.position_at(prompt.caret).y - bounds.top();
            scroll.clamp(
                (caret_y + LINE_HEIGHT - bounds.size.height.max(LINE_HEIGHT)).max(px(0.)),
                caret_y,
            )
        } else {
            scroll
        };
        layout.scroll = scroll.min(layout.max_scroll());
        Some(layout)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut TextStyle,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(layout) = layout.take() else {
            return;
        };
        let prompt = self.0.read(cx);
        let focus = prompt.focus.clone();
        let selection = prompt.selection();
        let marked = prompt.marked.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.0.clone()), cx);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if focus.is_focused(window) && !selection.is_empty() {
                for row in layout.selection_rows(selection.clone()) {
                    window.paint_quad(fill(row, rgb(theme::FOCUS)));
                }
            }
            for line in &layout.lines {
                let origin = layout.origin() + point(px(0.), line.top);
                if origin.y < bounds.bottom()
                    && origin.y + line.shaped.size(LINE_HEIGHT).height > bounds.top()
                {
                    let _ =
                        line.shaped
                            .paint(origin, LINE_HEIGHT, TextAlign::Left, None, window, cx);
                }
            }
            if let Some(marked) = marked {
                for row in layout.selection_rows(marked) {
                    let line = Bounds::new(
                        point(row.left(), row.bottom() - px(2.)),
                        size(row.size.width, px(1.)),
                    );
                    window.paint_quad(fill(line, rgb(theme::MUTED)));
                }
            }
            if focus.is_focused(window) && selection.is_empty() {
                window.paint_quad(fill(layout.range_bounds(selection), rgb(theme::TEXT)));
            }
        });
        self.0.update(cx, |prompt, _| prompt.layout = Some(layout));
    }
}
