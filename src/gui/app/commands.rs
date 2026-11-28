use std::sync::Arc;

use aiface::connector::ToolCallMode;
use gpui::{AnyView, Context, Entity, EventEmitter, Subscription, Window, prelude::*};
use morpheus::{
    ModelEntry, assembly::config::AgentConfig, commands::Command, config, setup::Setup,
    tools_config::save_tool_call_mode,
};

use super::Desktop;
use crate::{
    routes::{
        Saved,
        preferences::{Page, Preferences},
        providers::Providers,
        router::Router,
    },
    ui::command_dialog::{CommandDialog, Confirmed},
};

pub(super) struct Route {
    pub(super) title: &'static str,
    pub(super) view: AnyView,
    _saved: Subscription,
}

pub(super) struct ActiveDialog {
    pub(super) view: Entity<CommandDialog>,
    _confirmed: Subscription,
}

fn unavailable(command: Command) -> Option<&'static str> {
    match command {
        Command::Agents | Command::Confidence => {
            Some("External agent panes are available in the terminal.")
        }
        Command::Undo => Some("Session revert points are available in the terminal."),
        Command::Download => Some("The local model installer is available in the terminal."),
        Command::Voice => Some("Automatic speech is available in voice-enabled terminal builds."),
        Command::Themes
        | Command::Background
        | Command::Wallpaper
        | Command::Bell
        | Command::Spinner => Some("This command controls the terminal interface."),
        Command::New
        | Command::Models
        | Command::Compact
        | Command::Export
        | Command::Router
        | Command::Providers
        | Command::Settings
        | Command::Tools
        | Command::ToolCall
        | Command::Rename
        | Command::Worktree => None,
    }
}

pub(crate) fn offers_command(command: Command) -> bool {
    unavailable(command).is_none()
}

impl Desktop {
    pub(super) fn run_command(
        &mut self,
        command: Command,
        args: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = self.chats[self.active].clone();
        if let Some(message) = unavailable(command) {
            chat.update(cx, |chat, cx| chat.notice(message, cx));
            return;
        }

        match (command, args) {
            (Command::Models, "") => chat.update(cx, |chat, cx| chat.open_models(window, cx)),
            (Command::Models, args) => self.select_model(args, cx),
            (Command::Worktree, args) => self.run_worktree(args, window, cx),
            (_, args) if !args.is_empty() => {
                let name = command.metadata().0;
                chat.update(cx, |chat, cx| {
                    chat.notice(format!("Use /{name} on its own."), cx)
                });
            }
            (Command::New, _) => self.open_chat(window, cx),
            (Command::Compact, _) => chat.update(cx, |chat, cx| chat.compact(cx)),
            (Command::Export, _) => chat.update(cx, |chat, cx| chat.export(cx)),
            (Command::Router, _) => {
                let view = cx.new(Router::new);
                view.read(cx).focus(window, cx);
                self.show_route("Model routing", view, window, cx);
            }
            (Command::Providers, _) => {
                let view = cx.new(|cx| Providers::new(window, cx));
                view.read(cx).focus(window, cx);
                self.show_route("Providers", view, window, cx);
            }
            (Command::Settings, _) => self.open_preferences(Page::Settings, window, cx),
            (Command::Tools, _) => {
                let cwd = chat.read(cx).cwd().to_owned();
                self.open_preferences(Page::Tools(cwd), window, cx);
            }
            (Command::ToolCall, _) => {
                let mode = chat.read(cx).tool_call_mode();
                self.show_dialog(window, cx, |cx| CommandDialog::tool_call(mode, cx));
            }
            (Command::Rename, _) => {
                let title = chat.read(cx).title().to_owned();
                self.show_dialog(window, cx, |cx| CommandDialog::rename(title, cx));
            }
            _ => unreachable!("unsupported desktop command handled above"),
        }
    }

    fn select_model(&mut self, args: &str, cx: &mut Context<Self>) {
        let chat = &self.chats[self.active];
        let mut words = args.split_whitespace();
        let model = match (words.next(), words.next(), words.next()) {
            (Some("auto"), None, None) => ModelEntry {
                provider: String::new(),
                model: "auto".into(),
            },
            (Some(provider), Some(model), None) if model != "auto" => {
                let provider = provider.to_ascii_lowercase();
                if !aiface::connector::known_providers().any(|name| name == provider) {
                    chat.update(cx, |chat, cx| {
                        chat.notice(format!("Unknown provider {provider}."), cx)
                    });
                    return;
                }
                ModelEntry {
                    provider,
                    model: model.into(),
                }
            }
            _ => {
                chat.update(cx, |chat, cx| {
                    chat.notice("Use /models <provider> <model> or /models auto.", cx);
                });
                return;
            }
        };
        chat.update(cx, |chat, cx| {
            let reasoning = chat
                .matches_model(&model)
                .then(|| chat.reasoning())
                .flatten()
                .map(str::to_owned);
            chat.apply_model(&model, reasoning, cx);
        });
    }

    pub(super) fn choose_model(
        &mut self,
        model: ModelEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = &self.chats[self.active];
        let levels = if model.provider.is_empty() {
            config::auto_reasoning_levels()
        } else if config::model_supports_reasoning(&model.model) {
            config::model_reasoning_levels(&model.model)
        } else {
            chat.update(cx, |chat, cx| chat.apply_model(&model, None, cx));
            return;
        };
        let chat = chat.read(cx);
        let current = chat
            .matches_model(&model)
            .then(|| {
                levels
                    .iter()
                    .position(|level| level == chat.reasoning().unwrap_or("default"))
            })
            .flatten();
        self.show_dialog(window, cx, |cx| {
            CommandDialog::reasoning(model, levels, current, cx)
        });
    }

    fn reload_configuration(&mut self, setup: &Setup, cx: &mut Context<Self>) {
        morpheus::harness::lsp::set_lsp_enabled(setup.lsp);
        self.config = Arc::new(AgentConfig::from_setup(setup, &self.config.cwd));
        for chat in &self.chats {
            chat.update(cx, |chat, _| chat.refresh_config(setup, &self.config));
        }
    }

    fn open_preferences(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        let title = page.title();
        let view = cx.new(|cx| Preferences::new(page, cx));
        view.read(cx).focus(window);
        self.show_route(title, view, window, cx);
    }

    fn show_route<V: Render + EventEmitter<Saved>>(
        &mut self,
        title: &'static str,
        view: Entity<V>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let saved = cx.subscribe_in(&view, window, |desktop, _, _, _, cx| {
            desktop.reload_configuration(&Setup::load(), cx);
        });
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

    pub(super) fn show_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        create: impl FnOnce(&mut Context<CommandDialog>) -> CommandDialog,
    ) -> Entity<CommandDialog> {
        let view = cx.new(create);
        let confirmed = cx.subscribe_in(&view, window, |desktop, view, event, window, cx| {
            if desktop
                .dialog
                .as_ref()
                .is_some_and(|dialog| &dialog.view == view)
            {
                desktop.confirm_dialog(view, event, window, cx);
            }
        });
        view.read(cx).focus(window, cx);
        self.dialog = Some(ActiveDialog {
            view: view.clone(),
            _confirmed: confirmed,
        });
        cx.notify();
        view
    }

    fn confirm_dialog(
        &mut self,
        view: &Entity<CommandDialog>,
        event: &Confirmed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = &self.chats[self.active];
        match event {
            Confirmed::Rename(title) => chat.update(cx, |chat, cx| chat.rename(title.clone(), cx)),
            Confirmed::Model(model, effort) => {
                chat.update(cx, |chat, cx| chat.apply_model(model, effort.clone(), cx));
            }
            Confirmed::ToolCall(mode) => {
                let mut setup = Setup::load();
                if !save_tool_call_mode(&mut setup, *mode) {
                    view.update(cx, |view, cx| {
                        view.feedback("Could not save the tool call mode.", cx)
                    });
                    return;
                }
                self.reload_configuration(&Setup::load(), cx);
                let message = match mode {
                    ToolCallMode::Native => "Native tool calls selected.",
                    ToolCallMode::Inline => "Inline tool calls selected.",
                };
                self.chats[self.active].update(cx, |chat, cx| chat.notice(message, cx));
            }
            Confirmed::Close => {}
        }
        self.close_dialog(window, cx);
    }

    pub(super) fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.dialog.take().is_none() {
            return false;
        }
        self.chats[self.active].read(cx).focus_prompt(window, cx);
        cx.notify();
        true
    }
}
