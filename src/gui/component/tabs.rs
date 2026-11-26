use gpui::{
    App, ClickEvent, Div, EntityId, PathBuilder, ScrollHandle, Stateful, Window, canvas, div,
    point, prelude::*, px, rgb,
};

use crate::theme;

pub(crate) fn bar() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(50.))
        .px(px(8.))
        .flex_shrink_0()
        .border_b_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::CHROME))
}

pub(crate) fn strip(scroll: &ScrollHandle) -> Stateful<Div> {
    div()
        .id("tab-strip")
        .flex()
        .items_center()
        .gap(px(4.))
        .min_w_0()
        .overflow_x_scroll()
        .track_scroll(scroll)
}

pub(crate) fn tab(
    id: EntityId,
    active: bool,
    select: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(("chat-tab", id))
        .flex()
        .flex_shrink_0()
        .items_center()
        .w(px(120.))
        .h(px(34.))
        .pr(px(4.))
        .rounded(px(7.))
        .when(active, |tab| tab.bg(rgb(theme::ACTIVE)))
        .when(!active, |tab| {
            tab.hover(|style| style.bg(rgb(theme::SURFACE)))
        })
        .child(
            div()
                .id("select-tab")
                .flex()
                .items_center()
                .flex_1()
                .h_full()
                .pl(px(12.))
                .cursor_pointer()
                .child(icon(
                    Icon::Chat,
                    if active { theme::TEXT } else { theme::MUTED },
                ))
                .on_click(select),
        )
        .child(
            icon_button("close-tab", Icon::Close)
                .size(px(24.))
                .on_click(close),
        )
}

pub(crate) fn new_button() -> Stateful<Div> {
    icon_button("new-tab", Icon::Add).size(px(32.))
}

fn icon_button(id: &'static str, glyph: Icon) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .cursor_pointer()
        .rounded(px(5.))
        .hover(|style| style.bg(rgb(theme::HOVER)))
        .child(icon(glyph, theme::MUTED))
}

enum Icon {
    Add,
    Close,
    Chat,
}

fn icon(glyph: Icon, color: u32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let mut path = PathBuilder::stroke(px(1.4));
            let at = |x, y| bounds.origin + point(px(x), px(y));
            match glyph {
                Icon::Add => {
                    path.move_to(at(3., 8.));
                    path.line_to(at(13., 8.));
                    path.move_to(at(8., 3.));
                    path.line_to(at(8., 13.));
                }
                Icon::Close => {
                    path.move_to(at(5., 5.));
                    path.line_to(at(11., 11.));
                    path.move_to(at(5., 11.));
                    path.line_to(at(11., 5.));
                }
                Icon::Chat => {
                    path.move_to(at(2., 3.));
                    path.line_to(at(14., 3.));
                    path.line_to(at(14., 11.));
                    path.line_to(at(6., 11.));
                    path.line_to(at(2., 14.));
                    path.close();
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(color));
            }
        },
    )
    .size(px(16.))
}
