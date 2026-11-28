use gpui::{Div, Styled, div, px, rgb};

use crate::theme;

pub(crate) fn render() -> Div {
    div()
        .w(px(208.))
        .flex_shrink_0()
        .border_r_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::CHROME))
}
