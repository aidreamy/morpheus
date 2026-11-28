pub(crate) mod preferences;
pub(crate) mod providers;
pub(crate) mod router;
pub(crate) mod session;

use aiface::connector::ToolCallMode;
use gpui::{Div, ElementId, Stateful, div, prelude::*, px, rgb};

use crate::theme;

pub(crate) enum Saved {
    Configuration,
    Rename(String),
    ToolCall(ToolCallMode),
}

pub(super) fn button(
    id: impl Into<ElementId>,
    text: impl Into<gpui::SharedString>,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .px(px(10.))
        .py(px(6.))
        .rounded(px(6.))
        .bg(rgb(theme::SURFACE))
        .hover(|style| style.bg(rgb(theme::HOVER)))
        .cursor_pointer()
        .text_size(px(13.))
        .child(text.into())
}

pub(super) fn content() -> Stateful<Div> {
    div()
        .id("route-content")
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
}

pub(super) fn section() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .w_full()
        .max_w(px(760.))
        .min_w_0()
        .mx(gpui::auto())
        .p(px(20.))
}

pub(super) fn row(label: impl Into<gpui::SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_w_0()
        .p(px(9.))
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(14.))
                .child(label.into()),
        )
}

pub(super) fn message(text: impl Into<gpui::SharedString>) -> Div {
    div()
        .text_size(px(13.))
        .text_color(rgb(theme::MUTED))
        .child(text.into())
}
