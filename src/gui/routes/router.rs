use std::time::Duration;

use gpui::{Context, Entity, EventEmitter, Window, div, prelude::*, px, uniform_list};
use morpheus::{
    ModelEntry,
    assembly::{agent_runtime, config::AgentConfig},
    fallback,
    models::{active_providers, list_models, matches_filter},
    setup::{FallbackEntry, Setup},
};

use super::{Saved, button, content, message, row, section};
use crate::component::prompt::{Prompt, Submitted};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Chain {
    Auto,
    Summary,
}

pub(crate) struct Router {
    setup: Setup,
    chain: Chain,
    filter: Entity<Prompt>,
    models: Vec<ModelEntry>,
    loading: bool,
    feedback: Option<String>,
    dirty_auto: bool,
    dirty_summary: bool,
}

impl EventEmitter<Saved> for Router {}

impl Router {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let mut setup = Setup::load();
        setup.routing.fallbacks = fallback::load_fallbacks(&setup);
        let base_urls = AgentConfig::from_setup(&setup, "").local_base_urls;
        let filter = cx.new(|cx| Prompt::field("Filter models", String::new(), false, cx));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&filter, |router, _, Submitted(query), cx| {
            let mut matches = router
                .models
                .iter()
                .filter(|entry| matches_filter(entry, query.trim()));
            if let (Some(entry), None) = (matches.next(), matches.next()) {
                router.add(entry.clone(), cx);
            }
        })
        .detach();

        let worker = agent_runtime().spawn(async move {
            list_models(
                active_providers(&base_urls),
                &base_urls,
                Some(Duration::from_secs(6)),
            )
            .await
        });
        cx.spawn(async move |router, cx| {
            let models = worker.await.unwrap_or_default();
            let _ = router.update(cx, |router, cx| {
                router.models = models;
                router.loading = false;
                cx.notify();
            });
        })
        .detach();
        Self {
            setup,
            chain: Chain::Auto,
            filter,
            models: Vec::new(),
            loading: true,
            feedback: None,
            dirty_auto: false,
            dirty_summary: false,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &gpui::App) {
        self.filter.read(cx).focus(window);
    }

    fn entries(&self) -> &[FallbackEntry] {
        match self.chain {
            Chain::Auto => &self.setup.routing.fallbacks,
            Chain::Summary => &self.setup.routing.summarization_models,
        }
    }

    fn entries_mut(&mut self) -> &mut Vec<FallbackEntry> {
        match self.chain {
            Chain::Auto => &mut self.setup.routing.fallbacks,
            Chain::Summary => &mut self.setup.routing.summarization_models,
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        match self.chain {
            Chain::Auto => self.dirty_auto = true,
            Chain::Summary => self.dirty_summary = true,
        }
        self.feedback = None;
        cx.notify();
    }

    fn add(&mut self, model: ModelEntry, cx: &mut Context<Self>) {
        let entries = self.entries_mut();
        if entries
            .iter()
            .any(|entry| entry.provider == model.provider && entry.model == model.model)
        {
            return;
        }
        entries.push(FallbackEntry {
            provider: model.provider,
            model: model.model,
        });
        self.changed(cx);
    }

    fn move_entry(&mut self, index: usize, step: isize, cx: &mut Context<Self>) {
        let entries = self.entries_mut();
        if let Some(other) = index
            .checked_add_signed(step)
            .filter(|other| index < entries.len() && *other < entries.len())
        {
            entries.swap(index, other);
            self.changed(cx);
        }
    }

    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.entries().len() {
            return;
        }
        if self.chain == Chain::Auto && self.entries().len() == 1 {
            self.feedback = Some("The automatic route needs one fallback.".into());
            cx.notify();
        } else {
            self.entries_mut().remove(index);
            self.changed(cx);
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let mut setup = Setup::load();
        if self.dirty_auto {
            setup
                .routing
                .fallbacks
                .clone_from(&self.setup.routing.fallbacks);
        }
        if self.dirty_summary {
            setup
                .routing
                .summarization_models
                .clone_from(&self.setup.routing.summarization_models);
        }
        if setup.save() {
            setup.routing.fallbacks = fallback::load_fallbacks(&setup);
            self.setup = setup;
            self.dirty_auto = false;
            self.dirty_summary = false;
            self.feedback = Some("Routes saved.".into());
            cx.emit(Saved);
        } else {
            self.feedback = Some("Could not save routes.".into());
        }
        cx.notify();
    }
}

impl Render for Router {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = self.entries();
        let query = self.filter.read(cx).value().trim();
        let matches: Vec<_> = self
            .models
            .iter()
            .filter(|entry| matches_filter(entry, query))
            .filter(|model| {
                !entries
                    .iter()
                    .any(|entry| entry.provider == model.provider && entry.model == model.model)
            })
            .cloned()
            .collect();
        let view = cx.entity().downgrade();
        let list_height =
            (px(46.) * matches.len()).min((window.viewport_size().height * 0.35).max(px(46.)));
        let controls = div()
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .children(
                [
                    (Chain::Auto, "Automatic fallbacks"),
                    (Chain::Summary, "Summarization"),
                ]
                .into_iter()
                .map(|(chain, label)| {
                    button(label, label)
                        .when(self.chain == chain, |tab| {
                            tab.bg(gpui::rgb(crate::theme::ACTIVE))
                        })
                        .on_click(cx.listener(move |router, _, _, cx| {
                            router.chain = chain;
                            cx.notify();
                        }))
                }),
            )
            .child(
                button("save-routes", "Save")
                    .on_click(cx.listener(|router, _, _, cx| router.save(cx))),
            );

        content().child(
            section()
                .child(controls)
                .children(self.feedback.clone().map(message))
                .when(entries.is_empty(), |body| {
                    body.child(message("Uses the active model for summarization."))
                })
                .children(entries.iter().enumerate().map(|(index, entry)| {
                    row(format!(
                        "{}. {} / {}",
                        index + 1,
                        entry.provider,
                        entry.model
                    ))
                    .children((index > 0).then(|| {
                        button(("fallback-up", index), "↑").on_click(
                            cx.listener(move |router, _, _, cx| router.move_entry(index, -1, cx)),
                        )
                    }))
                    .children((index + 1 < entries.len()).then(|| {
                        button(("fallback-down", index), "↓").on_click(
                            cx.listener(move |router, _, _, cx| router.move_entry(index, 1, cx)),
                        )
                    }))
                    .child(
                        button(("fallback-remove", index), "Remove").on_click(
                            cx.listener(move |router, _, _, cx| router.remove(index, cx)),
                        ),
                    )
                }))
                .child(self.filter.clone())
                .when(matches.is_empty(), |body| {
                    body.child(message(if self.loading {
                        "Loading models…"
                    } else {
                        "No models available to add."
                    }))
                })
                .when(!matches.is_empty(), |body| {
                    body.child(
                        uniform_list("router-models", matches.len(), move |range, _, _| {
                            range
                                .map(|index| {
                                    let model = matches[index].clone();
                                    let view = view.clone();
                                    row(format!("{} / {}", model.provider, model.model))
                                        .h(px(46.))
                                        .child(button(("add-fallback", index), "Add").on_click(
                                            move |_, _, cx| {
                                                let _ = view.update(cx, |router, cx| {
                                                    router.add(model.clone(), cx)
                                                });
                                            },
                                        ))
                                })
                                .collect::<Vec<_>>()
                        })
                        .h(list_height)
                        .w_full(),
                    )
                }),
        )
    }
}
