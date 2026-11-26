//! App-side plumbing for the download router: opening it, starting a model
//! install on a worker thread, and folding the worker's events back into the
//! view (toasts, the decision mirror pin, the refreshed rows).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::App;
use crate::routes::download::{DownloadEvent, DownloadKind};
use crate::ui::toast::{ToastOptions, ToastVariant};

impl App {
    /// Open `/download`, rebuilding the rows so installs made outside the TUI
    /// (or by a previous run) show their real state.
    pub(super) fn open_download(&mut self) {
        self.download_view.refresh(&self.setup);
        self.show_download = true;
    }

    pub(super) fn render_download_view(&mut self, buf: &mut Buffer, area: Rect) {
        self.prompt_view.blur();
        self.download_view.render(buf, area, &self.theme);
    }

    /// Drain the worker channel. Called every frame regardless of the active
    /// route, so a download finishes and toasts even after the user leaves the
    /// router.
    pub(super) fn poll_download(&mut self) {
        if !self.download_view.is_running() {
            return;
        }
        let label = self
            .download_view
            .active_label()
            .unwrap_or("Model")
            .to_string();
        let events = self.download_view.poll();
        let mut terminal = false;
        for event in events {
            match event {
                DownloadEvent::Progress { .. } => {}
                DownloadEvent::Finished { repo, revision } => {
                    terminal = true;
                    // A mirror install records the resolved SHA as the pin:
                    // every later load cache-probes exactly this snapshot.
                    if let (Some(repo), Some(revision)) = (repo, revision)
                        && repo == self.setup.decision.hub.effective_repo()
                    {
                        self.setup.decision.hub.pinned_repo = repo;
                        self.setup.decision.hub.pinned_revision = revision;
                        self.setup.save();
                    }
                    self.toast_state.show(ToastOptions {
                        title: Some("Download".into()),
                        message: format!("{label} installed."),
                        variant: ToastVariant::Success,
                        duration_ms: 4000,
                    });
                }
                DownloadEvent::Failed(reason) => {
                    terminal = true;
                    self.toast_state.show(ToastOptions {
                        title: Some("Download".into()),
                        message: format!("{label}: {reason}"),
                        variant: ToastVariant::Error,
                        duration_ms: 8000,
                    });
                }
            }
        }
        if terminal {
            self.download_view.refresh(&self.setup);
        }
    }

    /// Enter/click on the row under the cursor.
    pub(super) fn activate_download_row(&mut self) {
        let Some((kind, installed)) = self.download_view.selected() else {
            return;
        };
        if installed {
            let label = self
                .download_view
                .selected_label()
                .unwrap_or("Model")
                .to_string();
            self.toast_state.show(ToastOptions {
                title: Some("Download".into()),
                message: format!("{label} is already installed."),
                variant: ToastVariant::Info,
                duration_ms: 3000,
            });
            return;
        }
        self.start_download(kind);
    }

    /// Install `kind` in the background. The worker owns the download and
    /// reports progress through a channel; the render loop never blocks.
    pub(super) fn start_download(&mut self, kind: DownloadKind) {
        if self.download_view.is_running() {
            self.toast_state.show(ToastOptions {
                title: Some("Download".into()),
                message: "A model download is already running.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 3000,
            });
            return;
        }
        #[cfg(not(any(feature = "speak", feature = "onnx", feature = "embed")))]
        {
            let _ = kind;
            return;
        }
        #[cfg(any(feature = "speak", feature = "onnx", feature = "embed"))]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            match kind {
                #[cfg(feature = "speak")]
                DownloadKind::Voice(dir) => {
                    let Some(bundle) = morpheus::speak::install::BUNDLES
                        .iter()
                        .find(|bundle| bundle.dir == dir)
                    else {
                        return;
                    };
                    let models = morpheus::speak::models_dir();
                    std::thread::spawn(move || {
                        let progress_tx = tx.clone();
                        let result =
                            morpheus::speak::install::download(bundle, &models, |done, total| {
                                let _ = progress_tx.send(DownloadEvent::Progress { done, total });
                            });
                        let _ = tx.send(match result {
                            Ok(_) => DownloadEvent::Finished {
                                repo: None,
                                revision: None,
                            },
                            Err(reason) => DownloadEvent::Failed(reason),
                        });
                    });
                }
                #[cfg(feature = "onnx")]
                DownloadKind::Decision => {
                    let Some(target) = morpheus::weights::resolve_target(
                        &self.setup.decision.model,
                        &self.setup.decision.hub,
                    ) else {
                        return;
                    };
                    let repo = target.repo.clone();
                    std::thread::spawn(move || {
                        let progress_tx = tx.clone();
                        let result = morpheus::weights::install(&target, move |done, total| {
                            let _ = progress_tx.send(DownloadEvent::Progress { done, total });
                        });
                        let _ = tx.send(match result {
                            Ok(revision) => DownloadEvent::Finished {
                                repo: Some(repo),
                                revision: Some(revision),
                            },
                            Err(reason) => DownloadEvent::Failed(reason),
                        });
                    });
                }
                #[cfg(feature = "embed")]
                DownloadKind::Embed(model) => {
                    std::thread::spawn(move || {
                        let result = morpheus_recall::embed::download_local_model(
                            model.to_fastembed_model(),
                        );
                        let _ = tx.send(match result {
                            Ok(()) => DownloadEvent::Finished {
                                repo: None,
                                revision: None,
                            },
                            Err(error) => DownloadEvent::Failed(error.to_string()),
                        });
                    });
                }
            }
            self.download_view.start(kind, rx);
        }
    }
}
