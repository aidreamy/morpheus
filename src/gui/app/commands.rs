use std::sync::Arc;

use gpui::{AnyView, Context, Entity, EventEmitter, Subscription, Window, prelude::*};
use morpheus::{assembly::config::AgentConfig, commands::Command, setup::Setup};

use super::Desktop;
use crate::routes::{
    Saved,
    preferences::{Page, Preferences},
    providers::Providers,
    router::Router,
};

pub(super) struct Route {
    pub(super) title: &'static str,
    pub(super) view: AnyView,
    _saved: Subscription,
}

impl Desktop {
    pub(super) fn run_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = self.chats[self.active].clone();
        match command {
            Command::New => self.open_chat(window, cx),
            Command::Models => chat.update(cx, |chat, cx| chat.open_models(window, cx)),
            Command::Compact => chat.update(cx, |chat, cx| chat.compact(cx)),
            Command::Export => chat.update(cx, |chat, cx| chat.export(cx)),
            Command::Router => {
                let view = cx.new(Router::new);
                view.read(cx).focus(window, cx);
                self.show_route("Model routing", view, window, cx);
            }
            Command::Providers => {
                let view = cx.new(|cx| Providers::new(window, cx));
                view.read(cx).focus(window, cx);
                self.show_route("Providers", view, window, cx);
            }
            Command::Settings => self.open_preferences(Page::Settings, window, cx),
            Command::Tools => {
                self.open_preferences(Page::Tools(self.config.cwd.clone()), window, cx)
            }
            Command::ToolCall => {
                self.open_preferences(Page::ToolCall(chat.read(cx).tool_call_mode()), window, cx)
            }
            Command::Rename => {
                self.open_preferences(Page::Rename(chat.read(cx).title().into()), window, cx)
            }
            _ => {}
        }
    }

    fn open_preferences(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        let title = page.title();
        let view = cx.new(|cx| Preferences::new(page, cx));
        view.read(cx).focus(window, cx);
        self.show_route(title, view, window, cx);
    }

    fn show_route<V: Render + EventEmitter<Saved>>(
        &mut self,
        title: &'static str,
        view: Entity<V>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let owner = self.chats[self.active].downgrade();
        let saved = cx.subscribe_in(
            &view,
            window,
            move |desktop, _, event, window, cx| match event {
                Saved::Configuration => {
                    let setup = Setup::load();
                    morpheus::harness::lsp::set_lsp_enabled(setup.lsp);
                    desktop.config = Arc::new(AgentConfig::from_setup(&setup, &desktop.config.cwd));
                    for chat in &desktop.chats {
                        chat.update(cx, |chat, _| chat.refresh_config(&desktop.config));
                    }
                }
                Saved::Rename(name) => {
                    let _ = owner.update(cx, |chat, cx| chat.rename(name.clone(), cx));
                    desktop.close_route(window, cx);
                }
                Saved::ToolCall(mode) => {
                    let _ = owner.update(cx, |chat, cx| chat.set_tool_call_mode(*mode, cx));
                    desktop.close_route(window, cx);
                }
            },
        );
        self.route = Some(Route {
            title,
            view: view.into(),
            _saved: saved,
        });
        cx.notify();
    }

    pub(super) fn close_route(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.route.take().is_none() {
            return false;
        }
        self.chats[self.active].read(cx).focus_prompt(window, cx);
        cx.notify();
        true
    }
}
