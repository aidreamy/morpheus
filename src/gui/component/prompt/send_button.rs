use gpui::{App, ClickEvent, PathBuilder, Window, canvas, div, point, prelude::*, px, rgb};

use crate::theme;

pub(super) fn render(
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("send-prompt")
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .size(px(36.))
        .rounded(px(11.))
        .bg(rgb(theme::TEXT))
        .hover(|style| style.bg(rgb(0xf0f0f0)))
        .block_mouse_except_scroll()
        .cursor_pointer()
        .on_click(on_click)
        .child(
            canvas(
                |_, _, _| (),
                |bounds, (), window, _| {
                    let mut path = PathBuilder::fill();
                    let at = |x, y| bounds.origin + point(px(x), px(y));
                    path.move_to(at(15.5, 2.5));
                    path.line_to(at(8., 12.5));
                    path.line_to(at(2.5, 9.5));
                    path.close();
                    path.move_to(at(16.75, 3.25));
                    path.line_to(at(14.5, 17.));
                    path.line_to(at(9.5, 13.5));
                    path.close();
                    path.move_to(at(5.5, 13.75));
                    path.line_to(at(6.5, 14.5));
                    path.line_to(at(3.5, 17.5));
                    path.line_to(at(2.5, 16.75));
                    path.close();
                    if let Ok(path) = path.build() {
                        window.paint_path(path, rgb(theme::SURFACE));
                    }
                },
            )
            .size(px(20.)),
        )
}
