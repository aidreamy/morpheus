use gpui::{FontWeight, Window, div, prelude::*, px, rgb};

use super::Desktop;
use crate::{component::tabs, keymap::*, left_panel, theme};

impl Render for Desktop {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strip = tabs::strip(&self.tab_scroll).children(self.chats.iter().enumerate().map(
            |(index, chat)| {
                let id = chat.entity_id();
                tabs::tab(
                    id,
                    chat.read(cx).title().to_owned(),
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

        let body = if let Some(route) = &self.route {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .flex_shrink_0()
                        .px(px(20.))
                        .py(px(12.))
                        .border_b_1()
                        .border_color(rgb(theme::BORDER))
                        .child(
                            div()
                                .id("back-to-chat")
                                .cursor_pointer()
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(5.))
                                .hover(|style| style.bg(rgb(theme::HOVER)))
                                .text_color(rgb(theme::MUTED))
                                .text_size(px(13.))
                                .child("← Chat")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_route(window, cx);
                                })),
                        )
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(route.title),
                        ),
                )
                .child(route.view.clone())
        } else {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .when(self.sidebar_visible, |body| {
                    body.child(left_panel::render())
                })
                .child(self.chats[self.active].clone())
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::CANVAS))
            .text_color(rgb(theme::TEXT))
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
                this.close_route(window, cx);
                this.chats[this.active].update(cx, |chat, cx| chat.toggle_commands(window, cx));
            }))
            .on_action(cx.listener(|this, command: &RunCommand, window, cx| {
                this.run_command(command.0, window, cx);
            }))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                if !this.close_route(window, cx) {
                    this.chats[this.active].update(cx, |chat, cx| chat.cancel(cx));
                }
            }))
            .child(
                tabs::bar().child(strip).child(
                    tabs::new_button()
                        .on_click(cx.listener(|this, _, window, cx| this.open_chat(window, cx))),
                ),
            )
            .child(body)
    }
}
