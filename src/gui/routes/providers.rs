use aiface::connector::{get_provider, invalidate_api_key, is_local_provider, known_providers};
use gpui::{Context, Entity, EventEmitter, ScrollHandle, Window, div, point, prelude::*, px};
use morpheus::{
    assembly::agent_runtime,
    providers::{provider_env_var_name, save_provider_api_key},
    setup::{LocalEndpoint, Setup},
};

use super::{Saved, button, content, message, row, section};
use crate::component::prompt::{Prompt, Submitted};

pub(crate) struct Providers {
    filter: Entity<Prompt>,
    scroll: ScrollHandle,
    setup: Setup,
    names: Vec<&'static str>,
    editing: Option<(&'static str, Entity<Prompt>)>,
    saving: bool,
    feedback: Option<String>,
}

impl EventEmitter<Saved> for Providers {}

impl Providers {
    pub(crate) fn new(window: &Window, cx: &mut Context<Self>) -> Self {
        let mut names: Vec<_> = known_providers().collect();
        names.sort_unstable();
        names.dedup();
        let filter = cx.new(|cx| Prompt::field("Filter providers", String::new(), false, cx));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&filter, window, |view, _, Submitted(query), window, cx| {
            let name = {
                let mut names = view.matching_names(query);
                names.next().filter(|_| names.next().is_none())
            };
            if let Some(name) = name {
                view.edit(name, window, cx);
            }
        })
        .detach();
        Self {
            filter,
            scroll: ScrollHandle::new(),
            setup: Setup::load(),
            names,
            editing: None,
            saving: false,
            feedback: None,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &gpui::App) {
        self.filter.read(cx).focus(window);
    }

    fn matching_names(&self, query: &str) -> impl Iterator<Item = &'static str> {
        let query = query.trim().to_ascii_lowercase();
        self.names
            .iter()
            .copied()
            .filter(move |name| name.contains(&query))
    }

    fn edit(&mut self, provider: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let local = is_local_provider(provider);
        let initial = if local {
            self.setup
                .local_base_url(provider)
                .or_else(|| get_provider(provider).map(|config| config.base_url))
                .unwrap_or_default()
        } else {
            ""
        };
        let field = cx.new(|cx| {
            Prompt::field(
                if local { "Server URL" } else { "API key" },
                initial.into(),
                !local,
                cx,
            )
        });
        cx.subscribe(&field, |view, emitter, Submitted(value), cx| {
            if view
                .editing
                .as_ref()
                .is_some_and(|(_, field)| field == &emitter)
            {
                view.save(value, cx);
            }
        })
        .detach();
        field.read(cx).focus(window);
        self.editing = Some((provider, field));
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.feedback = None;
        cx.notify();
    }

    fn save(&mut self, value: &str, cx: &mut Context<Self>) {
        let Some((provider, _)) = &self.editing else {
            return;
        };
        if self.saving {
            return;
        }
        let provider = *provider;
        let value = value.trim();
        if is_local_provider(provider) {
            let valid = url::Url::parse(value)
                .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some());
            if valid {
                let mut next = Setup::load();
                next.providers.local.insert(
                    provider.into(),
                    LocalEndpoint {
                        base_url: value.into(),
                    },
                );
                if next.save() {
                    self.setup = next;
                    self.feedback = Some(format!("Saved the endpoint for {provider}."));
                    cx.emit(Saved::Configuration);
                } else {
                    self.feedback = Some("Could not save the provider endpoint.".into());
                }
            } else {
                self.feedback = Some("Enter a valid HTTP or HTTPS server URL.".into());
            }
            cx.notify();
        } else if value.is_empty() {
            self.feedback = Some("Enter an API key.".into());
            cx.notify();
        } else {
            let key = value.to_owned();
            self.saving = true;
            self.feedback = Some("Saving to the system keyring…".into());
            cx.notify();
            let worker = agent_runtime().spawn_blocking(move || {
                let name = provider_env_var_name(provider);
                save_provider_api_key(&name, &key)?;
                invalidate_api_key(&name);
                Ok::<(), keyring::Error>(())
            });
            cx.spawn(async move |view, cx| {
                let outcome = worker.await;
                let _ = view.update(cx, |view, cx| {
                    view.saving = false;
                    view.feedback = Some(match &outcome {
                        Ok(Ok(())) => format!("Saved the API key for {provider}."),
                        Ok(Err(error)) => format!("Could not save the API key: {error}"),
                        Err(_) => "The keyring request failed.".into(),
                    });
                    if matches!(outcome, Ok(Ok(())))
                        && let Some((_, field)) = &view.editing
                    {
                        field.update(cx, |prompt, cx| prompt.clear_field(cx));
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }
}

impl Render for Providers {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = section().child(self.filter.clone());
        if let Some((name, field)) = &self.editing {
            let field = field.clone();
            let input = field.clone();
            body = body
                .child(
                    row(if is_local_provider(name) {
                        format!("{name} · Server URL")
                    } else {
                        format!("{name} · {}", provider_env_var_name(name))
                    })
                    .child(
                        button(
                            "save-provider",
                            if self.saving { "Saving…" } else { "Save" },
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let value = input.read(cx).value().to_owned();
                            this.save(&value, cx);
                        })),
                    ),
                )
                .child(field);
        }
        content().track_scroll(&self.scroll).child(
            body.children(self.feedback.clone().map(message)).child(
                div().flex().flex_col().gap(px(6.)).children(
                    self.matching_names(self.filter.read(cx).value())
                        .map(|name| {
                            let detail = if is_local_provider(name) {
                                self.setup
                                    .local_base_url(name)
                                    .unwrap_or("Local endpoint")
                                    .to_owned()
                            } else {
                                "API key".into()
                            };
                            row(format!("{name} · {detail}")).child(
                                button(
                                    gpui::SharedString::from(format!("provider-{name}")),
                                    "Configure",
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| this.edit(name, window, cx),
                                )),
                            )
                        }),
                ),
            ),
        )
    }
}
