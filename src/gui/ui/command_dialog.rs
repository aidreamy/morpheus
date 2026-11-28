use aiface::connector::ToolCallMode;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, FontWeight, Modifiers, ScrollHandle, Window,
    div, prelude::*, px, rgb,
};
use morpheus::ModelEntry;

use super::button;
use crate::{
    component::prompt::{Prompt, Submitted},
    theme,
};

#[derive(Clone)]
pub(crate) enum Confirmed {
    Rename(String),
    ToolCall(ToolCallMode),
    Model(ModelEntry, Option<String>),
    Close,
}

struct Choice {
    label: String,
    detail: Option<&'static str>,
    value: Confirmed,
}

enum Body {
    Rename(Entity<Prompt>),
    Choices {
        items: Vec<Choice>,
        current: Option<usize>,
        selected: usize,
    },
    Worktrees(Vec<String>),
}

pub(crate) struct CommandDialog {
    title: &'static str,
    focus: FocusHandle,
    scroll: ScrollHandle,
    body: Body,
    feedback: Option<String>,
}

impl EventEmitter<Confirmed> for CommandDialog {}

impl CommandDialog {
    fn new(title: &'static str, body: Body, cx: &mut Context<Self>) -> Self {
        Self {
            title,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            body,
            feedback: None,
        }
    }

    pub(crate) fn rename(title: String, cx: &mut Context<Self>) -> Self {
        let field = cx.new(|cx| {
            let mut prompt = Prompt::field("Session name", title, false, cx);
            prompt.select_all();
            prompt
        });
        cx.subscribe(&field, |dialog, _, Submitted(name), cx| {
            dialog.confirm_name(name, cx);
        })
        .detach();
        Self::new("Rename session", Body::Rename(field), cx)
    }

    pub(crate) fn tool_call(mode: ToolCallMode, cx: &mut Context<Self>) -> Self {
        let items = [
            (
                "Native",
                "Use structured tool calls from the provider.",
                ToolCallMode::Native,
            ),
            (
                "Inline",
                "Parse tool calls from JSON text in the response.",
                ToolCallMode::Inline,
            ),
        ]
        .into_iter()
        .map(|(label, detail, mode)| Choice {
            label: label.into(),
            detail: Some(detail),
            value: Confirmed::ToolCall(mode),
        })
        .collect();
        Self::choices(
            "Tool call mode",
            items,
            Some(usize::from(mode == ToolCallMode::Inline)),
            cx,
        )
    }

    pub(crate) fn reasoning(
        model: ModelEntry,
        levels: Vec<String>,
        current: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Self {
        let items = levels
            .into_iter()
            .map(|level| Choice {
                label: if level == "default" {
                    "Default".into()
                } else {
                    level.clone()
                },
                detail: None,
                value: Confirmed::Model(model.clone(), (level != "default").then_some(level)),
            })
            .collect();
        Self::choices("Reasoning effort", items, current, cx)
    }

    fn choices(
        title: &'static str,
        items: Vec<Choice>,
        current: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected = current.unwrap_or_default();
        let dialog = Self::new(
            title,
            Body::Choices {
                items,
                current,
                selected,
            },
            cx,
        );
        dialog.scroll.scroll_to_item(selected);
        dialog
    }

    pub(crate) fn worktrees(cx: &mut Context<Self>) -> Self {
        Self::new(
            "Git worktrees",
            Body::Worktrees(vec!["Loading worktrees…".into()]),
            cx,
        )
    }

    pub(crate) fn show_worktrees(&mut self, rows: Vec<String>, cx: &mut Context<Self>) {
        self.body = Body::Worktrees(rows);
        cx.notify();
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &App) {
        if let Body::Rename(field) = &self.body {
            field.read(cx).focus(window);
        } else {
            self.focus.focus(window);
        }
    }

    pub(crate) fn feedback(&mut self, message: &str, cx: &mut Context<Self>) {
        self.feedback = Some(message.into());
        cx.notify();
    }

    fn confirm_name(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        if name.is_empty() {
            self.feedback("Enter a session name.", cx);
        } else {
            cx.emit(Confirmed::Rename(name.into()));
        }
    }

    fn point_to_choice(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Body::Choices { selected, .. } = &mut self.body
            && *selected != index
        {
            *selected = index;
            cx.notify();
        }
    }
}

impl Render for CommandDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_picker = matches!(self.body, Body::Choices { .. });
        let scrollable = div()
            .id("command-dialog-options")
            .min_h_0()
            .max_h((window.viewport_size().height * 0.45).min(px(280.)))
            .overflow_y_scroll()
            .track_scroll(&self.scroll);
        let body = match &self.body {
            Body::Rename(field) => field.clone().into_any_element(),
            Body::Choices {
                items,
                current,
                selected,
            } => scrollable
                .flex()
                .flex_col()
                .gap(px(6.))
                .children(items.iter().enumerate().map(|(index, item)| {
                    let value = item.value.clone();
                    let is_current = *current == Some(index);
                    div()
                        .id(("dialog-option", index))
                        .w_full()
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap(px(12.))
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(theme::BORDER))
                        .bg(rgb(theme::SURFACE))
                        .cursor_pointer()
                        .when(*selected == index, |row| {
                            row.bg(rgb(theme::ACTIVE)).border_color(rgb(theme::FOCUS))
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .gap(px(2.))
                                .min_w_0()
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .line_height(px(20.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(item.label.clone()),
                                )
                                .children(
                                    item.detail.map(|detail| hint(detail).line_height(px(17.))),
                                ),
                        )
                        .children(is_current.then(|| hint("Current").flex_shrink_0()))
                        .on_mouse_move(cx.listener(move |dialog, _, _, cx| {
                            dialog.point_to_choice(index, cx);
                        }))
                        .on_click(cx.listener(move |dialog, _, _, cx| {
                            dialog.point_to_choice(index, cx);
                            cx.emit(value.clone());
                        }))
                }))
                .into_any_element(),
            Body::Worktrees(rows) => div()
                .flex()
                .flex_col()
                .min_h_0()
                .gap(px(8.))
                .child(
                    scrollable.children(
                        rows.iter()
                            .cloned()
                            .map(|row| div().px(px(8.)).py(px(5.)).text_size(px(13.)).child(row)),
                    ),
                )
                .child(hint(
                    "Use /worktree add [path] <branch> to attach or /worktree exit to return.",
                ))
                .into_any_element(),
        };

        let footer = div()
            .flex()
            .justify_end()
            .gap(px(8.))
            .child(
                button(
                    "dismiss-dialog",
                    if matches!(self.body, Body::Worktrees(_)) {
                        "Close"
                    } else {
                        "Cancel"
                    },
                )
                .when(is_picker, |button| {
                    button
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(theme::BORDER))
                        .bg(rgb(theme::CHROME))
                })
                .on_click(cx.listener(|_, _, _, cx| cx.emit(Confirmed::Close))),
            )
            .children(match &self.body {
                Body::Rename(field) => {
                    let field = field.clone();
                    Some(
                        button("confirm-rename", "Rename")
                            .bg(rgb(theme::ACTIVE))
                            .on_click(cx.listener(move |dialog, _, _, cx| {
                                let name = field.read(cx).value().to_owned();
                                dialog.confirm_name(&name, cx);
                            })),
                    )
                }
                _ => None,
            });

        div()
            .id("command-dialog")
            .track_focus(&self.focus)
            .occlude()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(480.))
            .max_h_full()
            .gap(px(12.))
            .p(px(16.))
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(theme::FOCUS))
            .bg(rgb(theme::CHROME))
            .shadow_md()
            .when(is_picker, |panel| {
                panel
                    .max_w(px(432.))
                    .gap(px(16.))
                    .p(px(20.))
                    .rounded(px(16.))
                    .border_color(rgb(0x414141))
                    .shadow_lg()
            })
            .on_key_down(cx.listener(|dialog, event: &gpui::KeyDownEvent, _, cx| {
                let Body::Choices {
                    items, selected, ..
                } = &mut dialog.body
                else {
                    return;
                };
                if event.keystroke.modifiers != Modifiers::none() {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "up" => *selected = (*selected + items.len() - 1) % items.len(),
                    "down" => *selected = (*selected + 1) % items.len(),
                    "enter" if !event.is_held => cx.emit(items[*selected].value.clone()),
                    _ => return,
                }
                dialog.scroll.scroll_to_item(*selected);
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                div()
                    .text_size(px(if is_picker { 18. } else { 16. }))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title),
            )
            .child(body)
            .children(
                self.feedback
                    .clone()
                    .map(|feedback| hint(feedback).text_color(rgb(theme::TEXT))),
            )
            .child(footer)
    }
}

fn hint(text: impl Into<gpui::SharedString>) -> gpui::Div {
    div()
        .text_size(px(12.))
        .text_color(rgb(theme::MUTED))
        .child(text.into())
}
