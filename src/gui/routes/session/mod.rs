mod right_panel;
mod transcript;

use gpui::{App, Entity, ScrollHandle, Subscription, Window, div, prelude::*, px};
use morpheus::types::{Message, MessageRole, Part, TextPart, now_ms};

use crate::component::prompt::{Prompt, Submitted};

const CONTENT_WIDTH: f32 = 760.;
const CONTENT_PADDING: f32 = 24.;

/// A tab owns its chat state for the lifetime of the tab.
pub(crate) struct Chat {
    prompt: Entity<Prompt>,
    _prompt_submission: Subscription,
    messages: Vec<Message>,
    transcript_scroll: ScrollHandle,
}

impl Chat {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self::with_messages(Vec::new(), cx)
    }

    pub(crate) fn with_messages(messages: Vec<Message>, cx: &mut Context<Self>) -> Self {
        let prompt = cx.new(Prompt::new);
        let prompt_submission = cx.subscribe(&prompt, |chat, _, Submitted(text), cx| {
            chat.submit(text.clone(), cx);
        });
        Self {
            prompt,
            _prompt_submission: prompt_submission,
            messages,
            transcript_scroll: ScrollHandle::new(),
        }
    }

    pub(crate) fn focus_prompt(&self, window: &mut Window, cx: &App) {
        self.prompt.read(cx).focus(window);
    }

    fn submit(&mut self, text: String, cx: &mut Context<Self>) {
        let created_at = now_ms();
        let turn_id = format!("{created_at}-{}", self.messages.len());
        self.messages.extend([
            text_message(
                format!("msg-gui-{turn_id}"),
                MessageRole::User,
                text,
                created_at,
            ),
            text_message(
                format!("msg-err-gui-{turn_id}"),
                MessageRole::Assistant,
                "Error: This chat is not connected to an agent yet.".into(),
                created_at,
            ),
        ]);
        self.transcript_scroll.scroll_to_bottom();
        cx.notify();
    }
}

fn text_message(id: String, role: MessageRole, text: String, created_at: u64) -> Message {
    Message {
        id,
        role,
        parts: vec![Part::Text(TextPart {
            text,
            synthetic: false,
        })],
        created_at,
        agent: None,
        model: None,
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(transcript::render(&self.messages, &self.transcript_scroll))
            .child(
                div()
                    .flex()
                    .justify_center()
                    .px(px(CONTENT_PADDING))
                    .pb(px(CONTENT_PADDING))
                    .child(
                        div()
                            .w_full()
                            .max_w(px(CONTENT_WIDTH))
                            .child(self.prompt.clone()),
                    ),
            );

        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(column)
            .when(right_panel::fits(window), |chat| {
                chat.child(right_panel::render())
            })
    }
}
