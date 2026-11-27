#[cfg(any(feature = "speak", feature = "onnx", feature = "embed"))]
use std::sync::mpsc::{self, Sender};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use morpheus_tui::core::types::{MouseButton, MouseEvent, MouseEventType};

use super::{App, AppMode};
use crate::component::list_dialog::MouseHit;
use crate::routes::download::DownloadView;
#[cfg(any(feature = "speak", feature = "onnx", feature = "embed"))]
use crate::routes::download::{DownloadEvent, DownloadKind, DownloadResult};
use crate::ui::toast::{ToastOptions, ToastVariant};

impl App {
    pub(super) fn open_download(&mut self) {
        self.download_view.refresh(&self.setup);
        self.show_download = true;
    }

    pub(super) fn poll_download(&mut self) {
        let Some((label, result)) = self.download_view.poll() else {
            return;
        };
        match result {
            Ok(pin) => {
                if let Some((repo, revision)) = pin
                    && repo == self.setup.decision.hub.effective_repo()
                {
                    self.setup.decision.hub.pinned_repo = repo;
                    self.setup.decision.hub.pinned_revision = revision;
                    self.setup.save();
                }
                self.download_toast(format!("{label} installed."), ToastVariant::Success, 4000);
            }
            Err(reason) => {
                self.download_toast(format!("{label}: {reason}"), ToastVariant::Error, 8000);
            }
        }
        self.download_view.refresh(&self.setup);
    }

    pub(super) fn handle_download_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => self.download_view.select_prev(),
            KeyCode::Down => self.download_view.select_next(),
            KeyCode::Enter => self.activate_download(),
            KeyCode::Esc => self.dismiss_download(),
            KeyCode::Backspace => {
                let Some(picker) = self.download_view.picker() else {
                    return false;
                };
                picker.pop_char();
            }
            KeyCode::Char(ch)
                if matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) =>
            {
                if let Some(picker) = self.download_view.picker() {
                    picker.push_char(ch);
                } else if ch == ' ' {
                    self.activate_download();
                } else {
                    return false;
                }
            }
            _ => return false,
        }
        true
    }

    pub(super) fn dismiss_download(&mut self) {
        if !self.download_view.close_picker() {
            self.show_download = false;
            self.prompt_view.focus();
        }
    }

    pub(super) fn download_picker_visible(&self) -> bool {
        matches!(self.mode(), AppMode::Download)
            && !self.dialog.visible()
            && self.download_view.is_picker_open()
    }

    pub(super) fn handle_download_mouse(&mut self, x: u16, y: u16) -> bool {
        let area = self.settings_route_area(self.terminal_size());
        let MouseHit::Row(index) = self.download_view.hit_test(x, y, area) else {
            return false;
        };
        self.download_view.select_row(index);
        self.activate_download();
        true
    }

    pub(super) fn handle_download_picker_mouse(&mut self, mouse: &MouseEvent) {
        match mouse.event_type {
            MouseEventType::ScrollUp => self.download_view.select_prev(),
            MouseEventType::ScrollDown => self.download_view.select_next(),
            _ => {
                let confirm =
                    mouse.event_type == MouseEventType::Up && mouse.button == MouseButton::Left;
                match self
                    .download_view
                    .hit_test(mouse.x, mouse.y, self.terminal_size())
                {
                    MouseHit::Row(index) => {
                        self.download_view.select_row(index);
                        if confirm {
                            self.activate_download();
                        }
                    }
                    MouseHit::Outside | MouseHit::Dismiss if confirm => {
                        self.download_view.close_picker();
                    }
                    _ => {}
                }
            }
        }
    }

    fn activate_download(&mut self) {
        if !DownloadView::picker_fits(self.terminal_size()) {
            return;
        }
        if !self.download_view.is_picker_open() {
            self.download_view.open_picker();
        } else {
            #[cfg(any(feature = "speak", feature = "onnx", feature = "embed"))]
            self.start_download();
        }
    }

    #[cfg(any(feature = "speak", feature = "onnx", feature = "embed"))]
    fn start_download(&mut self) {
        let Some(model) = self.download_view.selected_model() else {
            return;
        };
        if model.installed {
            self.download_toast(
                format!("{} is already installed.", model.label),
                ToastVariant::Info,
                3000,
            );
            return;
        }
        if self.download_view.is_running() {
            self.download_toast(
                "A model download is already running.".into(),
                ToastVariant::Warning,
                3000,
            );
            return;
        }

        let kind = model.kind.clone();
        let worker_kind = kind.clone();
        let label = model.label.clone();
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("model-download".into())
            .spawn(move || {
                let result = install_model(worker_kind, tx.clone());
                let _ = tx.send(DownloadEvent::Finished(result));
            }) {
            Ok(_) => self.download_view.start(kind, label, rx),
            Err(error) => self.download_toast(
                format!("Could not start the model download: {error}"),
                ToastVariant::Error,
                8000,
            ),
        }
    }

    fn download_toast(&mut self, message: String, variant: ToastVariant, duration_ms: u64) {
        self.toast_state.show(ToastOptions {
            title: Some("Download".into()),
            message,
            variant,
            duration_ms,
        });
    }
}

#[cfg(any(feature = "speak", feature = "onnx", feature = "embed"))]
fn install_model(kind: DownloadKind, _events: Sender<DownloadEvent>) -> DownloadResult {
    #[cfg(any(feature = "speak", feature = "onnx"))]
    let progress = move |done, total| {
        let _ = _events.send(DownloadEvent::Progress { done, total });
    };

    match kind {
        #[cfg(feature = "speak")]
        DownloadKind::TextToSpeech(bundle) => {
            morpheus::speak::install::download(bundle, &morpheus::speak::models_dir(), progress)?;
            Ok(None)
        }
        #[cfg(feature = "onnx")]
        DownloadKind::Decision(target) => {
            let revision = morpheus::weights::install(&target, progress)?;
            Ok(Some((target.repo, revision)))
        }
        #[cfg(feature = "embed")]
        DownloadKind::Embedding(model) => {
            morpheus_recall::embed::download_local_model(model.to_fastembed_model())
                .map_err(|error| error.to_string())?;
            Ok(None)
        }
    }
}
