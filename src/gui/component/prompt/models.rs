use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    Context, CursorStyle, Div, MouseButton, Stateful, Window, div, prelude::*, px, relative, rgb,
    rgba,
};
use morpheus::{ModelEntry, assembly::agent_runtime, models};

use super::{Prompt, layout::LINE_HEIGHT};
use crate::{keymap::ChooseModel, theme};

pub(super) struct Draft {
    text: String,
    anchor: usize,
    caret: usize,
}

impl Prompt {
    pub(crate) fn open_models(
        &mut self,
        current: Option<ModelEntry>,
        base_urls: HashMap<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus(window);
        if self.model_draft.is_some() {
            return;
        }
        self.model_draft = Some(Draft {
            text: std::mem::take(&mut self.text),
            anchor: self.anchor,
            caret: self.caret,
        });
        self.reset_editing();
        cx.notify();

        if self.model_picker.open(current) {
            let worker = agent_runtime().spawn(async move {
                let providers = models::active_providers(&base_urls);
                models::list_models(providers, &base_urls, Some(Duration::from_secs(6))).await
            });
            cx.spawn(async move |prompt, cx| {
                let models = worker.await.unwrap_or_default();
                let _ = prompt.update(cx, |prompt, cx| {
                    let query = if prompt.model_draft.is_some() {
                        &prompt.text
                    } else {
                        ""
                    };
                    prompt.model_picker.loaded(models, query);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub(super) fn close_models(&mut self) -> bool {
        let Some(draft) = self.model_draft.take() else {
            return false;
        };
        self.reset_editing();
        self.text = draft.text;
        self.anchor = draft.anchor;
        self.caret = draft.caret;
        true
    }

    pub(super) fn pick_model(
        &mut self,
        model: ModelEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.close_models() {
            self.focus(window);
            window.dispatch_action(Box::new(ChooseModel(model)), cx);
            cx.notify();
        }
    }

    pub(super) fn render_draft(&self, draft: &Draft, cx: &mut Context<Self>) -> Stateful<Div> {
        let preview = draft
            .text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .to_owned();
        div()
            .id("prompt-draft")
            .cursor(CursorStyle::IBeam)
            .text_size(px(15.))
            .text_color(rgb(theme::TEXT))
            .child(
                div()
                    .w_full()
                    .h(LINE_HEIGHT)
                    .line_height(LINE_HEIGHT)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(preview),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|prompt, _, window, cx| {
                    prompt.close_models();
                    prompt.focus(window);
                    cx.notify();
                }),
            )
    }

    pub(super) fn render_models(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let search = self
            .render_input("model-search", cx)
            .flex_shrink_0()
            .relative()
            .min_h(LINE_HEIGHT + px(20.))
            .px(px(11.))
            .py(px(9.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgba(if focused { 0xffffff22 } else { 0xffffff16 }))
            .bg(rgb(theme::SURFACE))
            .when(self.text.is_empty(), |input| {
                input.child(
                    div()
                        .absolute()
                        .left(px(12.))
                        .top(px(9.))
                        .text_size(px(13.))
                        .line_height(LINE_HEIGHT)
                        .text_color(rgb(theme::MUTED))
                        .child("Search by model or provider"),
                )
            });
        // Reserve space for the tabs, composer, and the picker's own controls.
        let list_height = (window.viewport_size().height - px(320.)).min(px(300.));
        let prompt = cx.entity().downgrade();

        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(relative(1.))
            .pb(px(8.))
            .child(self.model_picker.render(
                &self.text,
                search,
                list_height,
                move |model, _, window, cx| {
                    let _ = prompt.update(cx, |prompt, cx| prompt.pick_model(model, window, cx));
                },
            ))
    }
}
