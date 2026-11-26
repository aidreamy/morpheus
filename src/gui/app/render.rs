use gpui::{Window, div, prelude::*, rgb};

use super::Desktop;
use crate::{component::tabs, keymap::*, left_panel, theme};

impl Render for Desktop {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strip = tabs::strip(&self.tab_scroll).children(self.chats.iter().enumerate().map(
            |(index, chat)| {
                let id = chat.entity_id();
                tabs::tab(
                    id,
                    index == self.active,
                    cx.listener(move |this, _, window, cx| {
                        if let Some(index) = this.chat_index(id) {
                            this.activate(index, window, cx);
                        }
                    }),
                    cx.listener(move |this, _, window, cx| {
                        if let Some(index) = this.chat_index(id) {
                            this.close_chat(index, window, cx);
                        }
                    }),
                )
            },
        ));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::CANVAS))
            .key_context("Desktop")
            .on_action(cx.listener(|this, _: &NewTab, window, cx| {
                this.open_chat(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.close_chat(this.active, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                this.activate((this.active + 1) % this.chats.len(), window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                this.activate(
                    (this.active + this.chats.len() - 1) % this.chats.len(),
                    window,
                    cx,
                );
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_visible = !this.sidebar_visible;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleRightPanel, _, cx| {
                this.chats[this.active].update(cx, |chat, cx| chat.toggle_right_panel(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleCommands, window, cx| {
                this.chats[this.active].update(cx, |chat, cx| chat.toggle_commands(window, cx));
            }))
            .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                this.chats[this.active].update(cx, |chat, cx| chat.cancel(cx));
            }))
            .on_action(cx.listener(|this, _: &StopTurn, _, cx| {
                this.chats[this.active].update(cx, |chat, cx| chat.stop(cx));
            }))
            .child(
                tabs::bar().child(strip).child(
                    tabs::new_button()
                        .on_click(cx.listener(|this, _, window, cx| this.open_chat(window, cx))),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_visible, |body| {
                        body.child(left_panel::render())
                    })
                    .child(self.chats[self.active].clone()),
            )
    }
}
