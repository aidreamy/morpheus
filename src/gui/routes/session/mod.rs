mod agent_loop;
mod commands;
mod permission;
mod right_panel;
mod transcript;

use std::sync::Arc;

use aiface::connector::ToolCallMode;
use gpui::{App, Entity, ScrollHandle, Subscription, Window, div, prelude::*, px, relative};
use morpheus::types::{Message, MessageRole, Part, TextPart, now_ms};
use morpheus::{
    ModelEntry, assembly::config::AgentConfig, harness::ContextManagerState,
    session::store::generate_session_id,
};

use crate::{
    component::prompt::{ModelSelected, Prompt, Submitted},
    theme,
};

use self::agent_loop::Turn;
use self::commands::Compaction;

const CONTENT_WIDTH: f32 = 760.;
const CONTENT_PADDING: f32 = 24.;

pub(super) const UNAVAILABLE_TOOLS: &[&str] = &[
    "ask_questions",
    "agents_call",
    "agents_compact",
    "agents_worktree",
    #[cfg(feature = "embed")]
    "recall_search",
];

/// A tab owns its chat state for the lifetime of the tab.
pub(crate) struct Chat {
    prompt: Entity<Prompt>,
    _prompt_submission: Subscription,
    _model_selection: Subscription,
    config: Arc<AgentConfig>,
    session_id: String,
    title: String,
    messages: Vec<Message>,
    context: Option<ContextManagerState>,
    transcript_scroll: ScrollHandle,
    right_panel_visible: bool,
    turn: Option<Turn>,
    compaction: Option<Compaction>,
    next_approval: u64,
    notice: Option<String>,
}

impl Chat {
    pub(crate) fn new(config: Arc<AgentConfig>, cx: &mut Context<Self>) -> Self {
        let prompt = cx.new(Prompt::new);
        let prompt_submission = cx.subscribe(&prompt, |chat, _, Submitted(text), cx| {
            chat.submit(text.clone(), cx);
        });
        let model_selection = cx.subscribe(&prompt, |chat, _, ModelSelected(model), cx| {
            chat.apply_model(model);
            cx.notify();
        });
        Self {
            prompt,
            _prompt_submission: prompt_submission,
            _model_selection: model_selection,
            config,
            session_id: generate_session_id(),
            title: String::new(),
            messages: Vec::new(),
            context: None,
            transcript_scroll: ScrollHandle::new(),
            right_panel_visible: true,
            turn: None,
            compaction: None,
            next_approval: 0,
            notice: None,
        }
    }

    pub(crate) fn focus_prompt(&self, window: &mut Window, cx: &App) {
        self.prompt.read(cx).focus(window);
    }

    pub(crate) fn toggle_right_panel(&mut self, cx: &mut Context<Self>) {
        self.right_panel_visible = !self.right_panel_visible;
        cx.notify();
    }

    pub(crate) fn toggle_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt
            .update(cx, |prompt, cx| prompt.toggle_commands(window, cx));
    }

    pub(crate) fn open_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.config.model.as_ref().map(|model| ModelEntry {
            provider: if model == "auto" {
                String::new()
            } else {
                self.config.provider.clone()
            },
            model: model.clone(),
        });
        let base_urls = self.config.local_base_urls.clone();
        self.prompt.update(cx, |prompt, cx| {
            prompt.open_models(current, base_urls, window, cx)
        });
    }

    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self
            .prompt
            .update(cx, |prompt, cx| prompt.dismiss_commands(cx))
        {
            self.stop(cx);
        }
    }

    fn submit(&mut self, text: String, cx: &mut Context<Self>) {
        if self.turn.is_some() || self.compaction.is_some() {
            return;
        }
        let mut words = text.split_whitespace();
        if words.next() == Some("/models") {
            self.select_model(words.next(), words.next(), words.next());
            cx.notify();
            return;
        }

        self.notice = None;
        self.push_text(MessageRole::User, text.clone());
        self.transcript_scroll.scroll_to_bottom();
        self.run_turn(text, cx);
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn rename(&mut self, title: String, cx: &mut Context<Self>) {
        self.title = title;
        cx.notify();
    }

    pub(crate) fn tool_call_mode(&self) -> ToolCallMode {
        self.config.tool_call_mode
    }

    pub(crate) fn set_tool_call_mode(&mut self, mode: ToolCallMode, cx: &mut Context<Self>) {
        Arc::make_mut(&mut self.config).tool_call_mode = mode;
        self.notice = Some(
            match mode {
                ToolCallMode::Native => "Native tool calls selected.",
                ToolCallMode::Inline => "Inline tool calls selected.",
            }
            .into(),
        );
        cx.notify();
    }

    pub(crate) fn refresh_config(&mut self, shared: &AgentConfig) {
        let config = &self.config;
        self.config = Arc::new(AgentConfig {
            provider: config.provider.clone(),
            model: config.model.clone(),
            reasoning: config.reasoning.clone(),
            tool_call_mode: config.tool_call_mode,
            base_url: shared.local_base_urls.get(&config.provider).cloned(),
            ..shared.clone()
        });
    }

    fn select_model(&mut self, provider: Option<&str>, model: Option<&str>, extra: Option<&str>) {
        match (provider, model, extra) {
            (Some("auto"), None, None) => self.apply_model(&ModelEntry {
                provider: String::new(),
                model: "auto".into(),
            }),
            (Some(provider), Some(model), None) if model != "auto" => {
                let provider = provider.to_ascii_lowercase();
                let known = aiface::connector::known_providers().any(|name| name == provider);
                if !known {
                    self.error(format!("Unknown provider: {provider}"));
                    return;
                }
                self.apply_model(&ModelEntry {
                    provider,
                    model: model.into(),
                });
            }
            _ => self.error("Usage: /models <provider> <model> or /models auto".into()),
        }
    }

    fn apply_model(&mut self, model: &ModelEntry) {
        let config = Arc::make_mut(&mut self.config);
        let changed = config.model.as_ref() != Some(&model.model)
            || (!model.provider.is_empty() && config.provider != model.provider);
        config.model = Some(model.model.clone());
        if changed {
            config.reasoning = None;
        }
        if model.provider.is_empty() {
            self.notice = Some("Model set to auto for this chat.".into());
        } else {
            config.base_url = config.local_base_urls.get(&model.provider).cloned();
            config.provider.clone_from(&model.provider);
            self.notice = Some(format!(
                "Model set to {} / {} for this chat.",
                model.provider, model.model
            ));
        }
    }

    fn push_text(&mut self, role: MessageRole, text: String) {
        self.messages.push(text_message(
            format!("msg-gui-{}", self.messages.len()),
            role,
            text,
        ));
    }

    fn error(&mut self, error: String) {
        self.messages.push(text_message(
            format!("msg-err-gui-{}", self.messages.len()),
            MessageRole::Assistant,
            format!("Error: {error}"),
        ));
    }
}

fn text_message(id: String, role: MessageRole, text: String) -> Message {
    Message {
        id,
        role,
        parts: vec![Part::Text(TextPart {
            text,
            synthetic: false,
        })],
        created_at: now_ms(),
        agent: None,
        model: None,
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let approval = self.turn.as_ref().and_then(|turn| turn.approval.as_ref());
        let status = self
            .turn
            .as_ref()
            .map(|turn| {
                if turn.stopping {
                    "Stopping…".into()
                } else if turn.approval.is_some() {
                    "Waiting for approval".into()
                } else if let Some(tool) = &turn.tool {
                    format!("Running {tool}…")
                } else {
                    "Working…".into()
                }
            })
            .or_else(|| {
                self.compaction
                    .as_ref()
                    .map(|_| "Compacting context…".into())
            })
            .or_else(|| self.notice.clone());
        let column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(transcript::render(&self.messages, &self.transcript_scroll))
            .child(
                div()
                    .flex()
                    .justify_center()
                    .max_h(relative(1.))
                    .px(px(CONTENT_PADDING))
                    .pb(px(CONTENT_PADDING))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .w_full()
                            .min_w_0()
                            .max_w(px(CONTENT_WIDTH))
                            .children(approval.map(|approval| {
                                let id = approval.id;
                                let view = cx.entity().downgrade();
                                permission::render(approval, move |action, _, _, cx| {
                                    let _ =
                                        view.update(cx, |chat, cx| chat.approve(id, action, cx));
                                })
                            }))
                            .children(status.map(|status| {
                                div()
                                    .flex_shrink_0()
                                    .mb(px(8.))
                                    .line_height(px(16.))
                                    .text_size(px(12.))
                                    .text_color(gpui::rgb(theme::MUTED))
                                    .child(status)
                            }))
                            .child(self.prompt.clone()),
                    ),
            );

        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(column)
            .when(
                self.right_panel_visible && right_panel::fits(window),
                |chat| chat.child(right_panel::render()),
            )
    }
}
