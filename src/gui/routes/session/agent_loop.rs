use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use gpui::Context;
use morpheus::assembly::{agent_runtime, assemble, run_assembled_loop};
use morpheus::harness::{
    ContextManagerState, HarnessEvent, Mode, PermissionAction, context::ContextItem,
};
use morpheus::types::{Message, MessageRole, Part};
use morpheus_tools::question::types::AnswerItem;
use tokio::sync::{Notify, mpsc};

use super::{Chat, UNAVAILABLE_TOOLS, text_message};

pub(super) struct Turn {
    stop: Arc<AtomicBool>,
    cancel_assembly: Arc<Notify>,
    answer: mpsc::UnboundedSender<Result<Vec<AnswerItem>, String>>,
    permission: mpsc::UnboundedSender<PermissionAction>,
    pub(super) approval: Option<Approval>,
    pub(super) tool: Option<String>,
    pub(super) stopping: bool,
    reply: ReplyStream,
}

pub(super) struct Approval {
    pub(super) id: u64,
    pub(super) tool: String,
    pub(super) description: String,
    pub(super) args: String,
}

#[derive(Default)]
struct ReplyStream {
    slot: Option<ReplySlot>,
    leading_text: String,
}

#[derive(Clone, Copy)]
enum ReplySlot {
    Streaming(usize),
    Retrying(usize),
}

impl ReplyStream {
    fn begin(&mut self) {
        if matches!(self.slot, Some(ReplySlot::Streaming(_))) {
            self.slot = None;
        }
        self.leading_text.clear();
    }

    fn retry(&mut self) {
        // Keep the old layout until the retry sends replacement text.
        if let Some(ReplySlot::Streaming(index)) = self.slot {
            self.slot = Some(ReplySlot::Retrying(index));
        }
        self.leading_text.clear();
    }

    fn append(&mut self, text: &str, messages: &mut Vec<Message>) -> bool {
        if text.is_empty() {
            return false;
        }
        if let Some(ReplySlot::Streaming(index)) = self.slot {
            if let Part::Text(part) = &mut messages[index].parts[0] {
                part.text.push_str(text);
            }
            return true;
        }

        self.leading_text.push_str(text);
        if text.trim().is_empty() {
            return false;
        }
        let text = std::mem::take(&mut self.leading_text);
        let index = if let Some(ReplySlot::Retrying(index)) = self.slot {
            if let Part::Text(part) = &mut messages[index].parts[0] {
                part.text = text;
            }
            index
        } else {
            let index = messages.len();
            messages.push(text_message(
                format!("msg-gui-{index}"),
                MessageRole::Assistant,
                text,
            ));
            index
        };
        self.slot = Some(ReplySlot::Streaming(index));
        true
    }

    fn discard_retry(&mut self, messages: &mut Vec<Message>) {
        if let Some(ReplySlot::Retrying(index)) = self.slot {
            messages.remove(index);
            self.slot = None;
        }
    }
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
            reply: ReplyStream::default(),
        });
        self.prompt
            .update(cx, |prompt, cx| prompt.set_busy(true, cx));

        let worker = agent_runtime().spawn(async move {
            let mode = match config.initial_mode {
                // Only the terminal exposes controls for these modes.
                Mode::Command | Mode::Yolo => Mode::Build,
                mode => mode,
            };
            let mut assembly = config.assembly(mode, context, Some(cache_key));
            assembly
                .disabled_tools
                .extend(UNAVAILABLE_TOOLS.iter().map(ToString::to_string));

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
        if let Some(compaction) = &self.compaction {
            compaction.cancel.notify_one();
        }
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
                if !turn.reply.append(&text, &mut self.messages) {
                    return false;
                }
            }
            HarnessEvent::BeginAssistant => {
                turn.reply.begin();
                return false;
            }
            HarnessEvent::ClearAssistant => {
                turn.reply.retry();
                return false;
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
                turn.reply.discard_retry(&mut self.messages);
                if turn.reply.slot.is_none()
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
        let stopping = self.turn.take().is_some_and(|mut turn| {
            turn.reply.discard_retry(&mut self.messages);
            turn.stopping
        });
        if let Some(error) = error.filter(|_| !stopping) {
            self.error(error);
        }
        self.prompt
            .update(cx, |prompt, cx| prompt.set_busy(false, cx));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(message: &Message) -> &str {
        let Part::Text(part) = &message.parts[0] else {
            panic!("expected an assistant text part");
        };
        &part.text
    }

    #[test]
    fn retry_replaces_its_partial_message_after_the_first_visible_delta() {
        let mut reply = ReplyStream::default();
        let mut messages = Vec::new();
        reply.append("Previous iteration", &mut messages);
        reply.begin();
        reply.append("Interrupted response", &mut messages);
        let id = messages[1].id.clone();

        reply.retry();
        reply.begin();
        assert!(!reply.append("    ", &mut messages));
        assert_eq!(text(&messages[1]), "Interrupted response");
        assert!(reply.append("code", &mut messages));
        reply.append("\nnext line", &mut messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(text(&messages[0]), "Previous iteration");
        assert_eq!(messages[1].id, id);
        assert_eq!(text(&messages[1]), "    code\nnext line");
    }

    #[test]
    fn terminal_cleanup_discards_only_an_unreplaced_attempt() {
        let mut reply = ReplyStream::default();
        let mut messages = Vec::new();
        reply.append("Previous iteration", &mut messages);
        reply.begin();
        reply.append("Interrupted response", &mut messages);
        reply.retry();
        reply.discard_retry(&mut messages);

        assert!(reply.slot.is_none());
        assert_eq!(messages.len(), 1);
        assert_eq!(text(&messages[0]), "Previous iteration");
    }
}
