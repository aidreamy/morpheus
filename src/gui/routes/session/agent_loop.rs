use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use gpui::Context;
use morpheus::assembly::{agent_runtime, assemble, run_assembled_loop};
use morpheus::harness::{
    ContextManagerState, HarnessEvent, Mode, PermissionAction, context::ContextItem,
};
use morpheus::types::{MessageRole, Part};
use morpheus_tools::question::types::AnswerItem;
use tokio::sync::{Notify, mpsc};

use super::{Chat, text_message};

pub(super) struct Turn {
    stop: Arc<AtomicBool>,
    cancel_assembly: Arc<Notify>,
    answer: mpsc::UnboundedSender<Result<Vec<AnswerItem>, String>>,
    permission: mpsc::UnboundedSender<PermissionAction>,
    pub(super) approval: Option<Approval>,
    pub(super) tool: Option<String>,
    pub(super) stopping: bool,
    reply: Option<usize>,
}

pub(super) struct Approval {
    pub(super) id: u64,
    pub(super) tool: String,
    pub(super) description: String,
    pub(super) args: String,
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.request_stop();
    }
}

impl Turn {
    fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.cancel_assembly.notify_one();
    }
}

impl Chat {
    pub(super) fn run_turn(&mut self, input: String, cx: &mut Context<Self>) {
        let config = self.config.clone();
        let context = self.context.clone();
        let cache_key = self.session_id.clone();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (answer, answer_rx) = mpsc::unbounded_channel();
        let (permission, perm_rx) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let cancel_assembly = Arc::new(Notify::new());
        self.turn = Some(Turn {
            stop: stop.clone(),
            cancel_assembly: cancel_assembly.clone(),
            answer,
            permission,
            approval: None,
            tool: None,
            stopping: false,
            reply: None,
        });
        self.prompt
            .update(cx, |prompt, cx| prompt.set_busy(true, cx));

        let worker = agent_runtime().spawn(async move {
            let mode = match config.initial_mode {
                Mode::Command => Mode::Build,
                mode => mode,
            };
            let mut assembly = config.assembly(mode, context, Some(cache_key));
            assembly.disabled_tools.extend(
                [
                    "ask_questions",
                    "agents_call",
                    "agents_compact",
                    "agents_worktree",
                ]
                .map(String::from),
            );

            let assembled = tokio::select! {
                biased;
                _ = cancel_assembly.notified() => return,
                result = assemble(assembly, &event_tx) => result,
            };
            let mut harness = match assembled {
                Ok(harness) => harness,
                Err(message) => {
                    let _ = event_tx.send(HarnessEvent::Error {
                        message,
                        context: None,
                        checkup_verdict: None,
                    });
                    return;
                }
            };
            run_assembled_loop(
                &mut harness,
                &input,
                event_tx,
                answer_rx,
                perm_rx,
                stop,
                None,
            )
            .await;
        });

        cx.spawn(async move |chat, cx| {
            while let Some(event) = event_rx.recv().await {
                match chat.update(cx, |chat, cx| chat.handle_agent_event(event, cx)) {
                    Ok(false) => {}
                    _ => return,
                }
            }
            let error = match worker.await {
                Ok(()) => "The agent ended without completing the turn.",
                Err(error) => {
                    log::error!("desktop agent task failed: {error}");
                    "The agent task ended unexpectedly."
                }
            };
            let _ = chat.update(cx, |chat, cx| {
                chat.finish_turn(None, Some(error.to_owned()), cx);
            });
        })
        .detach();
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(turn) = &mut self.turn {
            turn.stopping = true;
            turn.approval = None;
            turn.request_stop();
            cx.notify();
        }
    }

    pub(super) fn approve(&mut self, id: u64, action: PermissionAction, cx: &mut Context<Self>) {
        if let Some(turn) = &mut self.turn
            && turn
                .approval
                .as_ref()
                .is_some_and(|request| request.id == id)
        {
            turn.approval = None;
            let _ = turn.permission.send(action);
            cx.notify();
        }
    }

    fn handle_agent_event(&mut self, event: HarnessEvent, cx: &mut Context<Self>) -> bool {
        let Some(turn) = &mut self.turn else {
            return true;
        };
        match event {
            HarnessEvent::Token { text } => {
                let index = *turn.reply.get_or_insert_with(|| {
                    let index = self.messages.len();
                    self.messages.push(text_message(
                        format!("msg-gui-{index}"),
                        MessageRole::Assistant,
                        String::new(),
                    ));
                    index
                });
                if let Part::Text(part) = &mut self.messages[index].parts[0] {
                    part.text.push_str(&text);
                }
                self.transcript_scroll.scroll_to_bottom();
            }
            HarnessEvent::BeginAssistant => turn.reply = None,
            HarnessEvent::ClearAssistant => {
                if let Some(index) = turn.reply.take() {
                    self.messages.remove(index);
                }
            }
            HarnessEvent::PermissionRequest {
                tool,
                description,
                args,
            } => {
                if !turn.stopping {
                    self.next_approval += 1;
                    turn.approval = Some(Approval {
                        id: self.next_approval,
                        tool,
                        description,
                        args,
                    });
                }
            }
            HarnessEvent::QuestionRequest { .. } => {
                let _ = turn
                    .answer
                    .send(Err("The desktop interface has no question form.".into()));
                return false;
            }
            HarnessEvent::ToolCall { tool, .. } => turn.tool = Some(tool),
            HarnessEvent::ToolResult { .. } | HarnessEvent::ToolError { .. } => turn.tool = None,
            HarnessEvent::Done { context, .. } => {
                if turn.reply.is_none()
                    && let Some(ContextItem::Closure { content, .. }) = context.items.back()
                    && !content.trim().is_empty()
                    && !self.messages.last().is_some_and(|message| message.role == MessageRole::Assistant
                        && matches!(message.parts.first(), Some(morpheus::types::Part::Text(part)) if part.text == *content))
                {
                    self.push_text(MessageRole::Assistant, content.clone());
                }
                self.finish_turn(Some(context), None, cx);
                return true;
            }
            HarnessEvent::Stopped { context } => {
                self.finish_turn(Some(context), None, cx);
                return true;
            }
            HarnessEvent::Error {
                message, context, ..
            } => {
                self.finish_turn(context, Some(message), cx);
                return true;
            }
            _ => return false,
        }
        cx.notify();
        false
    }

    fn finish_turn(
        &mut self,
        context: Option<ContextManagerState>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(context) = context {
            self.context = Some(context);
        }
        let stopping = self.turn.take().is_some_and(|turn| turn.stopping);
        if let Some(error) = error.filter(|_| !stopping) {
            self.error(error);
        }
        self.prompt
            .update(cx, |prompt, cx| prompt.set_busy(false, cx));
        cx.notify();
    }
}
