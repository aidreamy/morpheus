use std::collections::HashSet;

use aiface::connector::ToolCallMode;
use gpui::{Context, Entity, EventEmitter, FocusHandle, Window, prelude::*};
use morpheus::{
    setup::Setup,
    tools_config::{internal_tools, load_disabled_tools, save_disabled_tools},
};

use super::{Saved, button, content, message, row, section};
use crate::component::prompt::{Prompt, Submitted};

pub(crate) enum Page {
    Settings,
    Tools(String),
    ToolCall(ToolCallMode),
    Rename(String),
}

impl Page {
    pub(crate) fn title(&self) -> &'static str {
        match self {
            Self::Settings => "Settings",
            Self::Tools(_) => "Tools",
            Self::ToolCall(_) => "Tool call mode",
            Self::Rename(_) => "Rename session",
        }
    }
}

pub(crate) struct Preferences {
    focus: FocusHandle,
    view: View,
    feedback: Option<String>,
}

enum View {
    Settings(Box<Setup>),
    Tools {
        setup: Box<Setup>,
        cwd: String,
        disabled: HashSet<String>,
    },
    ToolCall(ToolCallMode),
    Rename(Entity<Prompt>),
}

impl EventEmitter<Saved> for Preferences {}

impl Preferences {
    pub(crate) fn new(page: Page, cx: &mut Context<Self>) -> Self {
        let view = match page {
            Page::Settings => View::Settings(Box::new(Setup::load())),
            Page::Tools(cwd) => {
                let setup = Box::new(Setup::load());
                let disabled = load_disabled_tools(&setup, &cwd, setup.tools.is_global());
                View::Tools {
                    setup,
                    cwd,
                    disabled,
                }
            }
            Page::ToolCall(mode) => View::ToolCall(mode),
            Page::Rename(title) => {
                let field = cx.new(|cx| Prompt::field("Session name", title, false, cx));
                cx.subscribe(&field, |this, _, Submitted(name), cx| this.rename(name, cx))
                    .detach();
                View::Rename(field)
            }
        };
        Self {
            focus: cx.focus_handle(),
            view,
            feedback: None,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &gpui::App) {
        if let View::Rename(field) = &self.view {
            field.read(cx).focus(window);
        } else {
            self.focus.focus(window);
        }
    }

    fn rename(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        if name.is_empty() {
            self.feedback = Some("Enter a session name.".into());
            cx.notify();
        } else {
            cx.emit(Saved::Rename(name.into()));
        }
    }

    fn change_setting(&mut self, change: impl FnOnce(&mut Setup), cx: &mut Context<Self>) {
        let View::Settings(setup) = &mut self.view else {
            return;
        };
        let mut next = Box::new(Setup::load());
        change(&mut next);
        if next.save() {
            *setup = next;
            self.feedback = Some("Settings saved.".into());
            cx.emit(Saved::Configuration);
        } else {
            self.feedback = Some("Could not save settings.".into());
        }
        cx.notify();
    }

    fn change_scope(&mut self, cx: &mut Context<Self>) {
        let View::Tools {
            setup,
            cwd,
            disabled,
        } = &mut self.view
        else {
            return;
        };
        let mut next = Box::new(Setup::load());
        let scope = if setup.tools.is_global() {
            "project"
        } else {
            "global"
        };
        next.tools.scope = scope.into();
        if next.save() {
            *disabled = load_disabled_tools(&next, cwd, next.tools.is_global());
            *setup = next;
            self.feedback = Some("Tool scope saved.".into());
            cx.emit(Saved::Configuration);
        } else {
            self.feedback = Some("Could not save tool scope.".into());
        }
        cx.notify();
    }

    fn toggle_tool(&mut self, name: &str, cx: &mut Context<Self>) {
        let View::Tools {
            setup,
            cwd,
            disabled,
        } = &mut self.view
        else {
            return;
        };
        let mut next = Box::new(Setup::load());
        let global = next.tools.is_global();
        let mut names = load_disabled_tools(&next, cwd, global);
        if setup.tools.is_global() != global {
            *setup = next;
            *disabled = names;
            self.feedback = Some("The tool scope changed. Choose the tool again.".into());
            cx.notify();
            return;
        }
        if !names.remove(name) {
            names.insert(name.into());
        }
        match save_disabled_tools(&mut next, &names, cwd, global) {
            Ok(()) => {
                *disabled = names;
                *setup = next;
                self.feedback = None;
                cx.emit(Saved::Configuration);
            }
            Err(error) => self.feedback = Some(format!("Could not save tools: {error}")),
        }
        cx.notify();
    }
}

impl Render for Preferences {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = section().children(self.feedback.clone().map(message));
        let body = match &self.view {
            View::Settings(setup) => {
                type Setting = (&'static str, &'static str, bool, fn(&mut Setup));
                let settings: [Setting; 3] = [
                    ("lsp", "Language server integration", setup.lsp, |setup| {
                        setup.lsp = !setup.lsp
                    }),
                    (
                        "skills",
                        "Search skill directories recursively",
                        setup.skills.recursive,
                        |setup| setup.skills.recursive = !setup.skills.recursive,
                    ),
                    (
                        "context",
                        "Effective context budget",
                        setup.effective_context,
                        |setup| setup.effective_context = !setup.effective_context,
                    ),
                ];
                body.children(settings.into_iter().map(|(id, label, enabled, toggle)| {
                    row(label).child(button(id, if enabled { "On" } else { "Off" }).on_click(
                        cx.listener(move |this, _, _, cx| this.change_setting(toggle, cx)),
                    ))
                }))
            }
            View::Tools {
                setup, disabled, ..
            } => body
                .child(
                    row("Tool scope").child(
                        button(
                            "tool-scope",
                            if setup.tools.is_global() {
                                "Global"
                            } else {
                                "Project"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.change_scope(cx))),
                    ),
                )
                .children(internal_tools().iter().filter_map(|&(name, details)| {
                    if super::session::UNAVAILABLE_TOOLS.contains(&name) {
                        return None;
                    }
                    Some(row(format!("{name} · {details}")).child(
                        button(name, if disabled.contains(name) { "Off" } else { "On" }).on_click(
                            cx.listener(move |this, _, _, cx| this.toggle_tool(name, cx)),
                        ),
                    ))
                })),
            View::ToolCall(current) => body.children(
                [
                    (
                        "native",
                        "Native",
                        "Use the provider's tool-call format.",
                        ToolCallMode::Native,
                    ),
                    (
                        "inline",
                        "Inline",
                        "Parse tool calls from the model's response.",
                        ToolCallMode::Inline,
                    ),
                ]
                .into_iter()
                .map(|(id, name, description, mode)| {
                    row(format!("{name} · {description}")).child(
                        button(
                            id,
                            if *current == mode {
                                "Current"
                            } else {
                                "Choose"
                            },
                        )
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(Saved::ToolCall(mode)))),
                    )
                }),
            ),
            View::Rename(field) => {
                let name = field.clone();
                body.child(field.clone())
                    .child(button("rename-session", "Rename").on_click(cx.listener(
                        move |this, _, _, cx| {
                            let value = name.read(cx).value().to_owned();
                            this.rename(&value, cx);
                        },
                    )))
            }
        };

        content().track_focus(&self.focus).child(body)
    }
}
