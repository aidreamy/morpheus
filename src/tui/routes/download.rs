//! Download router — the single place that fetches local models.
//!
//! One `Models` section lists everything the build can run locally: the Kokoro
//! voice bundles (`speak`), the termination-checkup checkpoint (`onnx`) and the
//! embedding models the knowledge bases use (`embed`). Each row reports whether
//! its files are already on disk; Enter starts the download on a worker thread
//! and the footer shows live progress, so a slow transfer never blocks the
//! render loop or the chat.

use std::sync::mpsc::Receiver;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use morpheus_tui::core::types::MouseEvent;

use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line;
use crate::util::list_selection::ListSelection;

/// Lines each row occupies: the model name and its description.
const ROW_LINES: u16 = 2;
/// Section title, rule and the blank row under it.
const SECTION_TOP: u16 = 3;
/// A row never spans the whole terminal, so long descriptions stay readable.
const CONTENT_WIDTH: u16 = 78;

/// Which local model a row installs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadKind {
    /// A published Kokoro bundle, by directory name.
    #[cfg(feature = "speak")]
    Voice(&'static str),
    /// The configured termination-checkup checkpoint.
    #[cfg(feature = "onnx")]
    Decision,
    /// A local embedding model.
    #[cfg(feature = "embed")]
    Embed(morpheus_recall::embed::LocalEmbedModel),
}

/// What the worker thread reports back to the router.
pub enum DownloadEvent {
    Progress {
        done: u64,
        total: u64,
    },
    /// The files are installed. `repo`/`revision` are set for the decision
    /// checkpoint, whose mirror pin the app records when the install lands.
    Finished {
        repo: Option<String>,
        revision: Option<String>,
    },
    Failed(String),
}

/// One row of the `Models` section.
struct DownloadItem {
    label: String,
    detail: String,
    installed: bool,
    kind: DownloadKind,
}

/// The router's state: the item list, the row cursor and at most one
/// in-flight download.
pub struct DownloadView {
    pub selection: ListSelection,
    items: Vec<DownloadItem>,
    /// Index of the row whose download is running.
    active: Option<usize>,
    active_label: Option<String>,
    progress: Option<(u64, u64)>,
    receiver: Option<Receiver<DownloadEvent>>,
}

impl DownloadView {
    pub fn new(setup: &morpheus::setup::Setup) -> Self {
        Self {
            selection: ListSelection::new(),
            items: build_items(setup),
            active: None,
            active_label: None,
            progress: None,
            receiver: None,
        }
    }

    /// Rebuild the rows from the current configuration and files on disk.
    /// Called when the router opens and after every finished download.
    pub fn refresh(&mut self, setup: &morpheus::setup::Setup) {
        self.items = build_items(setup);
        self.selection.clamp(self.items.len());
    }

    pub fn select_next(&mut self, visible: usize) {
        self.selection.set_visible_count(visible);
        self.selection.select_next(self.items.len());
    }

    pub fn select_prev(&mut self, visible: usize) {
        self.selection.set_visible_count(visible);
        self.selection.select_prev(self.items.len());
    }

    /// The model under the cursor, with the row's install state.
    pub fn selected(&self) -> Option<(DownloadKind, bool)> {
        self.items
            .get(self.selection.selected_index)
            .map(|item| (item.kind, item.installed))
    }

    /// Label under the cursor, for the app's toasts.
    pub fn selected_label(&self) -> Option<&str> {
        self.items
            .get(self.selection.selected_index)
            .map(|item| item.label.as_str())
    }

    pub const fn is_running(&self) -> bool {
        self.receiver.is_some()
    }

    /// The model being downloaded, for the app's toasts.
    pub fn active_label(&self) -> Option<&str> {
        self.active_label.as_deref()
    }

    /// Attach the worker's event channel and mark `kind` as the active row.
    pub fn start(&mut self, kind: DownloadKind, receiver: Receiver<DownloadEvent>) {
        self.active = self.items.iter().position(|item| item.kind == kind);
        self.active_label = self
            .active
            .and_then(|index| self.items.get(index))
            .map(|item| item.label.clone());
        self.progress = None;
        self.receiver = Some(receiver);
    }

    /// Drain the worker's events; the caller reacts to them (toasts, mirror
    /// pin) and calls [`Self::refresh`] on completion.
    pub fn poll(&mut self) -> Vec<DownloadEvent> {
        let Some(receiver) = self.receiver.as_ref() else {
            return Vec::new();
        };
        let mut events = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(event) => {
                    if let DownloadEvent::Progress { done, total } = &event {
                        self.progress = Some((*done, *total));
                    }
                    events.push(event);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // A panicked worker must not leave the router stuck on
                    // "downloading" forever.
                    if !events
                        .iter()
                        .any(|event| !matches!(event, DownloadEvent::Progress { .. }))
                    {
                        events.push(DownloadEvent::Failed(
                            "the download stopped unexpectedly".into(),
                        ));
                    }
                    break;
                }
            }
        }
        if events
            .iter()
            .any(|event| !matches!(event, DownloadEvent::Progress { .. }))
        {
            self.receiver = None;
            self.active = None;
            self.active_label = None;
            self.progress = None;
        }
        events
    }

    /// Geometry shared by render and mouse dispatch: the content column and
    /// the first row's top.
    fn body(&self, area: Rect) -> (u16, u16, u16) {
        let width = area.width.min(CONTENT_WIDTH);
        let x = area.x + area.width.saturating_sub(width) / 2;
        (x, width, area.y + SECTION_TOP)
    }

    /// Rows that fit above the progress footer.
    fn visible_rows(&self, area: Rect) -> usize {
        area.height
            .saturating_sub(SECTION_TOP + 1)
            .saturating_div(ROW_LINES) as usize
    }

    /// The row under the pointer, when the click lands inside the content
    /// column.
    pub fn handle_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        let (x, width, top) = self.body(area);
        if mouse.x < x || mouse.x >= x + width || mouse.y < top {
            return None;
        }
        let row = self.selection.scroll_offset + ((mouse.y - top) / ROW_LINES) as usize;
        (row < self.items.len()).then_some(row)
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let (x, width, top) = self.body(area);
        if width == 0 || area.height <= SECTION_TOP {
            return;
        }
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);
        let installed = rgba_color(theme.success);

        draw_text_line(buf, "Models", x, area.y, width, Style::default().fg(fg));
        draw_text_line(
            buf,
            &"─".repeat(width as usize),
            x,
            area.y + 1,
            width,
            Style::default().fg(muted),
        );

        let visible = self.visible_rows(area);
        self.selection.set_visible_count(visible);
        self.selection.clamp(self.items.len());
        if self.items.is_empty() {
            draw_text_line(
                buf,
                "This build has no local models to download.",
                x,
                top,
                width,
                Style::default().fg(muted),
            );
            return;
        }
        for (offset, index) in (self.selection.scroll_offset
            ..(self.selection.scroll_offset + visible).min(self.items.len()))
            .enumerate()
        {
            let item = &self.items[index];
            let y = top + offset as u16 * ROW_LINES;
            let selected = index == self.selection.selected_index;
            let marker = if selected { "🞴 " } else { "  " };
            draw_text_line(
                buf,
                &format!("{marker}{}", item.label),
                x,
                y,
                width,
                Style::default().fg(if selected { primary } else { fg }),
            );
            let status = if self.active == Some(index) {
                "downloading"
            } else if item.installed {
                "installed"
            } else {
                "not installed"
            };
            // Right-aligned state, measured from the same text the label
            // draws so a long label never collides with it.
            let label_w = marker.chars().count() + item.label.chars().count();
            let status_x = x + width.saturating_sub(status.chars().count() as u16);
            if label_w < (status_x - x) as usize {
                draw_text_line(
                    buf,
                    status,
                    status_x,
                    y,
                    width,
                    Style::default().fg(if item.installed { installed } else { muted }),
                );
            }
            draw_text_line(
                buf,
                &format!("    {}", item.detail),
                x,
                y + 1,
                width,
                Style::default().fg(muted),
            );
        }

        if let Some((done, total)) = self.progress {
            let pct = done
                .min(total)
                .checked_mul(100)
                .and_then(|scaled| scaled.checked_div(total))
                .unwrap_or(0) as u8;
            let bar = crate::component::budget::render_budget_bar(pct);
            let mb = |bytes: u64| bytes / (1024 * 1024);
            let line = format!(
                "downloading {bar} {pct:>3}% · {}/{} MB",
                mb(done),
                mb(total)
            );
            draw_text_line(
                buf,
                &truncate(&line, width as usize),
                x,
                area.bottom().saturating_sub(1),
                width,
                Style::default().fg(primary),
            );
        }
    }
}

fn truncate(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

/// The rows this build supports, in section order.
fn build_items(setup: &morpheus::setup::Setup) -> Vec<DownloadItem> {
    #[cfg(not(any(feature = "speak", feature = "onnx", feature = "embed")))]
    {
        let _ = setup;
        return Vec::new();
    }
    #[cfg(any(feature = "speak", feature = "onnx", feature = "embed"))]
    {
        let mut items = Vec::new();
        #[cfg(feature = "speak")]
        {
            let models = morpheus::speak::models_dir();
            for bundle in morpheus::speak::install::BUNDLES {
                items.push(DownloadItem {
                    label: format!("{} · {} voices", bundle.label, bundle.voices),
                    detail: format!("Kokoro speech for {}", bundle.languages),
                    installed: bundle.is_installed(&models),
                    kind: DownloadKind::Voice(bundle.dir),
                });
            }
        }
        #[cfg(feature = "onnx")]
        {
            use morpheus::weights;
            if let Some(target) =
                weights::resolve_target(&setup.decision.model, &setup.decision.hub)
            {
                items.push(DownloadItem {
                    label: format!("{} · termination checkup", target.display_name),
                    detail: "Local decision model that reviews ambiguous loop stops".to_string(),
                    installed: weights::is_cached(&target),
                    kind: DownloadKind::Decision,
                });
            }
        }
        #[cfg(feature = "embed")]
        {
            for model in embedding_models() {
                items.push(DownloadItem {
                    label: model.label(),
                    detail: "Local embeddings for the knowledge bases".to_string(),
                    installed: morpheus_recall::embed::local_model_installed(
                        model.to_fastembed_model(),
                    ),
                    kind: DownloadKind::Embed(model),
                });
            }
        }
        items
    }
}

/// The local embedding models the knowledge bases actually use, without
/// duplicates; the default model stands in when no base is configured.
#[cfg(feature = "embed")]
fn embedding_models() -> Vec<morpheus_recall::embed::LocalEmbedModel> {
    use crate::routes::rag::models::EmbedderConfig;
    use crate::routes::rag::registry::RagRegistry;

    let mut models = Vec::new();
    for db in RagRegistry::load().dbs {
        if let EmbedderConfig::Local { model } = db.embedder
            && !models.contains(&model)
        {
            models.push(model);
        }
    }
    if models.is_empty() {
        models.push(morpheus_recall::embed::LocalEmbedModel::AllMiniLML6V2);
    }
    models
}
