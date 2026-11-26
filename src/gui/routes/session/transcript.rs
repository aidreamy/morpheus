use gpui::{ScrollHandle, SharedString, div, prelude::*, px, relative, rgb};
use morpheus::types::{Message, MessageRole, Part};

use super::{CONTENT_PADDING, CONTENT_WIDTH};
use crate::{theme, util::markdown};

pub(super) fn render(messages: &[Message], scroll: &ScrollHandle) -> impl IntoElement {
    // Use the previous layout to tell whether the reader is following the end.
    // GPUI then resolves the new bottom after laying out the text and footer.
    if scroll.offset().y + scroll.max_offset().height <= px(1.) {
        scroll.scroll_to_bottom();
    }

    div()
        .id("transcript")
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .child(
            div().flex().justify_center().p(px(CONTENT_PADDING)).child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(20.))
                    .w_full()
                    .max_w(px(CONTENT_WIDTH))
                    .min_w_0()
                    .text_size(px(15.))
                    .line_height(px(23.))
                    .text_color(rgb(theme::TEXT))
                    .children(messages.iter().filter_map(message)),
            ),
        )
}

fn message(message: &Message) -> Option<impl IntoElement> {
    let mut parts = message.parts.iter().filter_map(|part| match part {
        Part::Text(part) if !part.synthetic && !part.text.trim().is_empty() => Some(&part.text),
        _ => None,
    });
    let first = parts.next()?;
    let mut text = first.clone();
    for part in parts {
        text.push_str("\n\n");
        text.push_str(part);
    }

    let body = match message.role {
        MessageRole::User => sent(text).into_any_element(),
        // Stored sessions represent errors as assistant messages with this ID prefix.
        MessageRole::Assistant if message.id.starts_with("msg-err-") => {
            error(text).into_any_element()
        }
        MessageRole::Assistant => markdown::render(&text),
    };
    Some(
        div()
            .id(SharedString::from(message.id.clone()))
            .w_full()
            .child(body),
    )
}

fn sent(text: String) -> impl IntoElement {
    div().flex().justify_end().child(
        div()
            .min_w_0()
            .max_w(relative(0.84))
            .px(px(14.))
            .py(px(10.))
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(theme::HOVER))
            .bg(rgb(theme::SURFACE))
            .child(text),
    )
}

fn error(text: String) -> impl IntoElement {
    div()
        .min_w_0()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(9.))
        .border_1()
        .border_l_2()
        .border_color(rgb(0x845050))
        .bg(rgb(0x2b2020))
        .text_color(rgb(0xe7b4b4))
        .child(text)
}
