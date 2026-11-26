//! Dock for the active session's tasks, commands and subagent activity.

use gpui::{Div, Styled, Window, div, px, rgb};

use crate::theme;

pub(super) fn fits(window: &Window) -> bool {
    window.viewport_size().width >= px(800.)
}

pub(super) fn render() -> Div {
    div()
        .w(px(240.))
        .flex_shrink_0()
        .border_l_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::CHROME))
}
