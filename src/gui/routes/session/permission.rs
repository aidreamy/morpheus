use gpui::{App, ClickEvent, FontWeight, Window, div, prelude::*, px, rgb};
use morpheus::harness::PermissionAction;

use super::agent_loop::Approval;
use crate::theme;

pub(super) fn render<F>(approval: &Approval, on_answer: F) -> impl IntoElement
where
    F: Fn(PermissionAction, &ClickEvent, &mut Window, &mut App) + Clone + 'static,
{
    let mut card = div()
        .flex()
        .flex_col()
        .min_h(px(91.))
        .w_full()
        .mb(px(8.))
        .p(px(10.))
        .rounded(px(12.))
        .border_1()
        .border_color(rgb(theme::HOVER))
        .bg(rgb(theme::CHROME))
        .text_color(rgb(theme::TEXT));
    // Let the scrollable details give up space to the composer controls.
    card.style().flex_shrink = Some(2.);
    card.child(
        div()
            .flex_shrink_0()
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(14.))
            .line_height(px(18.))
            .child(format!("Allow {}?", approval.tool)),
    )
    .child(
        div()
            .id("permission-details")
            .mt(px(5.))
            .min_h(px(14.))
            .max_h(px(64.))
            .overflow_y_scroll()
            .text_size(px(12.))
            .line_height(px(14.))
            .text_color(rgb(theme::MUTED))
            .child(approval.description.clone())
            .child(approval.args.clone()),
    )
    .child(
        div()
            .flex()
            .flex_shrink_0()
            .justify_end()
            .gap(px(6.))
            .mt(px(8.))
            .children(
                [
                    ("Deny", PermissionAction::Deny),
                    ("Allow once", PermissionAction::AllowOnce),
                ]
                .into_iter()
                .map(|(label, action)| {
                    let on_answer = on_answer.clone();
                    div()
                        .id(label)
                        .px(px(9.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .bg(rgb(theme::SURFACE))
                        .hover(|style| style.bg(rgb(theme::HOVER)))
                        .child(label)
                        .on_click(move |event, window, cx| on_answer(action, event, window, cx))
                }),
            ),
    )
}
