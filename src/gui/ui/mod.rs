pub(crate) mod command_dialog;
pub(crate) mod model_picker;
pub(crate) mod slash_menu;
use gpui::{Div, ElementId, SharedString, Stateful, div, prelude::*, px, rgb};

use crate::theme;

pub(crate) fn button(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
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
