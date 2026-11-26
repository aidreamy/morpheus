use std::collections::HashMap;
use std::time::Duration;

use gpui::{Context, EventEmitter, Window};
use morpheus::{ModelEntry, assembly::agent_runtime, models};

use super::Prompt;

pub(crate) struct ModelSelected(pub(crate) ModelEntry);

impl EventEmitter<ModelSelected> for Prompt {}

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
            cx.emit(ModelSelected(model));
            cx.notify();
        }
    }
}
