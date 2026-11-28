use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

use gpui::Context;
use morpheus::{
    assembly::{agent_runtime, build_manual_compaction_harness},
    harness::ManualCompactionOutcome,
    session::export::write_transcript,
};
use tokio::sync::{Notify, mpsc};

use super::Chat;

pub(super) struct Compaction {
    pub(super) cancel: Arc<Notify>,
}

impl Drop for Compaction {
    fn drop(&mut self) {
        self.cancel.notify_one();
    }
}

impl Chat {
    pub(crate) fn compact(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() {
            self.notice = Some("Context compaction runs between turns.".into());
            cx.notify();
            return;
        }
        let Some(context) = self.context.clone() else {
            self.notice = Some("Nothing to compact yet.".into());
            cx.notify();
            return;
        };
        let config = self.config.clone();
        let cache_key = self.session_id.clone();
        let cancel = Arc::new(Notify::new());
        self.compaction = Some(Compaction {
            cancel: cancel.clone(),
        });
        self.notice = None;
        self.prompt
            .update(cx, |prompt, cx| prompt.set_busy(true, cx));
        cx.notify();

        let worker = agent_runtime().spawn(async move {
            let summary = if config.summarization_models.is_empty() && config.model.as_deref() == Some("auto") {
                config.fallbacks.clone()
            } else {
                config.summarization_models.clone()
            };
            let mut harness = build_manual_compaction_harness(
                &config.provider, config.model.as_deref(), config.base_url.clone(), summary,
                config.local_base_urls.clone(), config.anthropic_ttl_1h, config.openai_cache_retention.clone(),
                config.reasoning.clone(), &cache_key, &config.cwd,
            )?;
            harness.context_manager.restore_state(&context);
            let (events, _) = mpsc::unbounded_channel();
            let outcome = tokio::select! {
                biased;
                _ = cancel.notified() => return Ok(None),
                outcome = harness.compact_on_demand(&events, Arc::new(AtomicBool::new(false))) => outcome,
            };
            let state = harness.context_manager.save_state();
            Ok::<_, String>(Some((outcome, state)))
        });
        cx.spawn(async move |chat, cx| {
            let result = worker.await;
            let _ = chat.update(cx, |chat, cx| {
                chat.compaction = None;
                chat.prompt
                    .update(cx, |prompt, cx| prompt.set_busy(false, cx));
                chat.notice = Some(match result {
                    Ok(Ok(Some((ManualCompactionOutcome::Compacted, state)))) => {
                        chat.context = Some(state);
                        "Context compacted.".into()
                    }
                    Ok(Ok(Some((ManualCompactionOutcome::NothingToCompact, _)))) => {
                        "Nothing to compact yet.".into()
                    }
                    Ok(Ok(None)) => "Compaction stopped.".into(),
                    Ok(Err(error)) => format!("Could not compact context: {error}"),
                    _ => "Could not compact context.".into(),
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn export(&mut self, cx: &mut Context<Self>) {
        let Some(context) = self.context.clone() else {
            self.notice = Some("Nothing to export yet.".into());
            cx.notify();
            return;
        };
        let cwd = PathBuf::from(&self.config.cwd);
        let title = self.title.clone();
        let in_progress = self.turn.is_some() || self.compaction.is_some();
        let worker =
            agent_runtime().spawn_blocking(move || write_transcript(&context, &title, &cwd));
        cx.spawn(async move |chat, cx| {
            let result = worker.await;
            let _ = chat.update(cx, |chat, cx| {
                chat.notice = Some(match result {
                    Ok(Ok(path)) if in_progress => {
                        format!("Exported the last completed context to {}.", path.display())
                    }
                    Ok(Ok(path)) => format!("Exported transcript to {}.", path.display()),
                    Ok(Err(error)) => format!("Could not export transcript: {error}"),
                    Err(_) => "The transcript export failed.".into(),
                });
                cx.notify();
            });
        })
        .detach();
    }
}
