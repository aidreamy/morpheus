//! The /download route and its searchable local model picker.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;

use crate::component::list_dialog::{self, FilterState, ListBoxLayout, ListRow, MouseHit};
use crate::theme::{Theme, rgba_color};
use crate::util::draw::{
    SECTION_ITEM_INDENT, SECTION_PAD, draw_section_bottom, draw_section_top, draw_section_walls,
    draw_text_line_compact, wrap_words,
};
use crate::util::list_selection::ListSelection;
use morpheus::setup::Setup;

const SECTION_TITLE: &str = "Models";
const EMPTY_MESSAGE: &str = "This build has no local models to download.";
const STATUS_WIDTH: usize = "downloading 100%".len();

#[derive(Clone, PartialEq, Eq)]
pub enum DownloadKind {
    #[cfg(feature = "speak")]
    TextToSpeech(&'static morpheus::speak::install::Bundle),
    #[cfg(feature = "onnx")]
    Decision(morpheus::weights::WeightsTarget),
    #[cfg(feature = "embed")]
    Embedding(morpheus_recall::embed::LocalEmbedModel),
}

pub type DownloadResult = Result<Option<(String, String)>, String>;

#[cfg_attr(not(any(feature = "speak", feature = "onnx")), allow(dead_code))]
pub enum DownloadEvent {
    Progress { done: u64, total: u64 },
    Finished(DownloadResult),
}

pub struct DownloadModel {
    pub label: String,
    pub installed: bool,
    pub kind: DownloadKind,
    detail: String,
}

struct DownloadCategory {
    title: &'static str,
    description: &'static str,
    models: Vec<DownloadModel>,
}

struct DownloadJob {
    kind: DownloadKind,
    label: String,
    events: Receiver<DownloadEvent>,
    progress: Option<(u64, u64)>,
}

pub struct DownloadView {
    selection: ListSelection,
    categories: Vec<DownloadCategory>,
    picker: Option<FilterState>,
    job: Option<DownloadJob>,
}

impl DownloadView {
    pub fn new() -> Self {
        Self {
            selection: ListSelection::new(),
            categories: Vec::new(),
            picker: None,
            job: None,
        }
    }

    pub fn refresh(&mut self, setup: &Setup) {
        let title = self.category().map(|category| category.title);
        self.categories = build_categories(setup);
        if let Some(index) = self
            .categories
            .iter()
            .position(|category| Some(category.title) == title)
        {
            self.selection.selected_index = index;
        } else {
            self.picker = None;
        }
        self.selection.clamp(self.categories.len());

        if let Some(picker) = &self.picker {
            let selected = picker.clamped(self.matching_count());
            self.select_row(selected);
        }
    }

    pub fn select_next(&mut self) {
        let count = self.matching_count();
        if let Some(picker) = &mut self.picker {
            picker.select_next(count);
        } else {
            self.selection.select_next(self.categories.len());
        }
    }

    pub fn select_prev(&mut self) {
        let count = self.matching_count();
        if let Some(picker) = &mut self.picker {
            picker.select_prev(count);
        } else {
            self.selection.select_prev(self.categories.len());
        }
    }

    pub fn select_row(&mut self, index: usize) {
        if let Some(picker) = &mut self.picker {
            picker.selected = index;
        } else {
            self.selection.selected_index = index;
        }
    }

    pub fn picker(&mut self) -> Option<&mut FilterState> {
        self.picker.as_mut()
    }

    pub fn is_picker_open(&self) -> bool {
        self.picker.is_some()
    }

    pub fn open_picker(&mut self) {
        if self
            .category()
            .is_some_and(|category| !category.models.is_empty())
        {
            self.picker = Some(FilterState::new());
        }
    }

    pub fn close_picker(&mut self) -> bool {
        self.picker.take().is_some()
    }

    #[cfg(any(feature = "speak", feature = "onnx", feature = "embed"))]
    pub fn selected_model(&self) -> Option<&DownloadModel> {
        let picker = self.picker.as_ref()?;
        self.matching_models(picker).nth(picker.selected)
    }

    #[cfg(any(feature = "speak", feature = "onnx", feature = "embed"))]
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    #[cfg(any(feature = "speak", feature = "onnx", feature = "embed"))]
    pub fn start(&mut self, kind: DownloadKind, label: String, events: Receiver<DownloadEvent>) {
        self.job = Some(DownloadJob {
            kind,
            label,
            events,
            progress: None,
        });
    }

    pub fn poll(&mut self) -> Option<(String, DownloadResult)> {
        let job = self.job.as_mut()?;
        let result = loop {
            match job.events.try_recv() {
                Ok(DownloadEvent::Progress { done, total }) => job.progress = Some((done, total)),
                Ok(DownloadEvent::Finished(result)) => break result,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    break Err("the download stopped unexpectedly".into());
                }
            }
        };
        self.job.take().map(|job| (job.label, result))
    }

    pub fn hit_test(&self, x: u16, y: u16, area: Rect) -> MouseHit {
        if let Some(picker) = &self.picker {
            let count = self.matching_models(picker).count();
            let Some(layout) = picker_layout(picker, count, area) else {
                return MouseHit::Outside;
            };
            return match list_dialog::hit_test(&layout, x, y) {
                MouseHit::Row(index) if index >= count => MouseHit::Inside,
                hit => hit,
            };
        }

        let layout = self.layout(area);
        if x < layout.section.x.saturating_add(SECTION_PAD)
            || x >= layout.section.right().saturating_sub(SECTION_PAD)
        {
            return MouseHit::Outside;
        }
        layout
            .visible_lines()
            .find_map(|(row_y, line)| match line {
                Line::Category(index) if y == row_y => Some(MouseHit::Row(*index)),
                _ => None,
            })
            .unwrap_or(MouseHit::Outside)
    }

    pub fn picker_fits(area: Rect) -> bool {
        area.width >= 13 && area.height >= 5
    }

    fn category(&self) -> Option<&DownloadCategory> {
        self.categories.get(self.selection.selected_index)
    }

    fn matching_models<'a>(
        &'a self,
        picker: &'a FilterState,
    ) -> impl Iterator<Item = &'a DownloadModel> {
        self.category()
            .into_iter()
            .flat_map(|category| &category.models)
            .filter(|model| picker.matches(&model.label) || picker.matches(&model.detail))
    }

    fn matching_count(&self) -> usize {
        self.picker
            .as_ref()
            .map_or(0, |picker| self.matching_models(picker).count())
    }

    fn layout(&self, area: Rect) -> CategoryLayout {
        let content_width = self
            .categories
            .iter()
            .map(|category| {
                category.description.width().max(
                    usize::from(SECTION_ITEM_INDENT) + category.title.width() + 2 + STATUS_WIDTH,
                )
            })
            .max()
            .unwrap_or_else(|| EMPTY_MESSAGE.width());
        let width = (content_width + 2 * SECTION_PAD as usize).min(area.width as usize) as u16;
        let inner_width = width.saturating_sub(2 * SECTION_PAD) as usize;
        let mut lines = vec![Line::Title, Line::SectionTop];
        let mut selected_line = 0;
        if self.categories.is_empty() {
            lines.extend(
                wrap_words(EMPTY_MESSAGE, inner_width)
                    .into_iter()
                    .map(Line::Description),
            );
        }
        for (index, category) in self.categories.iter().enumerate() {
            if index > 0 {
                lines.push(Line::Blank);
            }
            lines.extend(
                wrap_words(category.description, inner_width)
                    .into_iter()
                    .map(Line::Description),
            );
            if index == self.selection.selected_index {
                selected_line = lines.len();
            }
            lines.push(Line::Category(index));
        }
        lines.push(Line::SectionBottom);

        let height = area.height.min(lines.len() as u16);
        let first_line = selected_line
            .saturating_sub(usize::from(height.saturating_sub(2)))
            .min(lines.len().saturating_sub(height as usize));
        CategoryLayout {
            section: Rect::new(
                area.x + (area.width - width) / 2,
                area.y + (area.height - height) / 2,
                width,
                height,
            ),
            lines,
            first_line,
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let fg = Style::default().fg(rgba_color(theme.text));
        let muted = Style::default().fg(rgba_color(theme.text_muted));
        let primary = Style::default().fg(rgba_color(theme.primary));
        let layout = self.layout(area);
        let section = layout.section;
        let x = section.x + SECTION_PAD.min(section.width);
        let inner_width = section.width.saturating_sub(2 * SECTION_PAD) as usize;

        for (y, line) in layout.visible_lines() {
            if matches!(line, Line::Blank | Line::Description(_) | Line::Category(_)) {
                draw_section_walls(buf, section.x, y, section.width, muted);
            }
            match line {
                Line::Title => {
                    buf.set_stringn(x, y, "Download", inner_width, muted);
                }
                Line::SectionTop => {
                    draw_section_top(buf, section.x, y, section.width, SECTION_TITLE, fg, muted);
                }
                Line::SectionBottom => {
                    draw_section_bottom(buf, section.x, y, section.width, muted);
                }
                Line::Blank => {}
                Line::Description(text) => {
                    buf.set_stringn(x, y, text, inner_width, muted);
                }
                Line::Category(index) => {
                    let category = &self.categories[*index];
                    let style = if *index == self.selection.selected_index {
                        primary
                    } else {
                        fg
                    };
                    let label = format!("{}: {}", category.title, self.category_status(category));
                    let label_width = inner_width.saturating_sub(SECTION_ITEM_INDENT as usize);
                    buf.set_stringn(
                        x.saturating_add(SECTION_ITEM_INDENT),
                        y,
                        label,
                        label_width,
                        style,
                    );
                }
            }
        }
    }

    fn category_status(&self, category: &DownloadCategory) -> String {
        if let Some(job) = &self.job
            && category.models.iter().any(|model| model.kind == job.kind)
        {
            return download_status(job.progress);
        }
        let installed = category
            .models
            .iter()
            .filter(|model| model.installed)
            .count();
        match category.models.len() {
            1 if installed == 1 => "installed".into(),
            1 => "not installed".into(),
            total => format!("{installed}/{total} installed"),
        }
    }

    pub fn render_picker(&self, buf: &mut Buffer, area: Rect, theme: &Theme, now: SystemTime) {
        let (Some(picker), Some(category)) = (&self.picker, self.category()) else {
            return;
        };
        let rows: Vec<_> = self
            .matching_models(picker)
            .map(|model| self.model_row(model))
            .collect();
        let Some(layout) = picker_layout(picker, rows.len(), area) else {
            return;
        };
        let title = format!("{} models", category.title);
        list_dialog::render_chrome(buf, &layout, theme, now, picker, &title, None);
        if rows.is_empty() {
            draw_text_line_compact(
                buf,
                "No matching models",
                layout.x + 1,
                layout.list_top(),
                layout.width.saturating_sub(2),
                Style::default().fg(rgba_color(theme.text_muted)),
            );
        } else {
            list_dialog::render_rows(buf, &layout, theme, &rows, picker.clamped(rows.len()));
        }
    }

    fn model_row(&self, model: &DownloadModel) -> ListRow {
        let status = match &self.job {
            Some(job) if job.kind == model.kind => download_status(job.progress),
            _ if model.installed => "installed".into(),
            _ => "not installed".into(),
        };
        let mut detail = format!("  ·  {status}");
        if !model.detail.is_empty() {
            detail.push_str(" · ");
            detail.push_str(&model.detail);
        }
        ListRow::item(&model.label, detail, model.installed)
    }
}

fn picker_layout(picker: &FilterState, count: usize, area: Rect) -> Option<ListBoxLayout> {
    DownloadView::picker_fits(area).then(|| {
        picker.layout(
            picker.clamped(count),
            count,
            usize::MAX,
            50,
            30,
            area,
            false,
            true,
        )
    })
}

struct CategoryLayout {
    section: Rect,
    lines: Vec<Line>,
    first_line: usize,
}

impl CategoryLayout {
    fn visible_lines(&self) -> impl Iterator<Item = (u16, &Line)> {
        (self.section.y..self.section.bottom()).zip(self.lines.iter().skip(self.first_line))
    }
}

enum Line {
    Title,
    SectionTop,
    SectionBottom,
    Blank,
    Description(String),
    Category(usize),
}

fn download_status(progress: Option<(u64, u64)>) -> String {
    match progress {
        Some((done, total)) if total > 0 => {
            let percent = u128::from(done.min(total)) * 100 / u128::from(total);
            format!("downloading {percent}%")
        }
        Some((done, _)) => format!("downloading {} MiB", done / (1024 * 1024)),
        None => "downloading".into(),
    }
}

fn build_categories(_setup: &Setup) -> Vec<DownloadCategory> {
    let categories: [Option<DownloadCategory>; _] = [
        #[cfg(feature = "speak")]
        Some(text_to_speech_category()),
        #[cfg(feature = "onnx")]
        decision_category(_setup),
        #[cfg(feature = "embed")]
        Some(embedding_category()),
    ];
    categories.into_iter().flatten().collect()
}

#[cfg(feature = "speak")]
fn text_to_speech_category() -> DownloadCategory {
    let models_dir = morpheus::speak::models_dir();
    DownloadCategory {
        title: "Text-to-speech",
        description: "Kokoro bundles with model weights and speaker data",
        models: morpheus::speak::install::BUNDLES
            .iter()
            .map(|bundle| DownloadModel {
                label: bundle.label.into(),
                detail: format!("{} voices, {}", bundle.voices, bundle.languages),
                installed: bundle.is_installed(&models_dir),
                kind: DownloadKind::TextToSpeech(bundle),
            })
            .collect(),
    }
}

#[cfg(feature = "onnx")]
fn decision_category(setup: &Setup) -> Option<DownloadCategory> {
    use morpheus::weights;

    let target = weights::resolve_target(&setup.decision.model, &setup.decision.hub)?;
    Some(DownloadCategory {
        title: "Decision",
        description: "Answer questions about structured data",
        models: vec![DownloadModel {
            label: target.display_name.clone(),
            detail: String::new(),
            installed: weights::is_cached(&target),
            kind: DownloadKind::Decision(target),
        }],
    })
}

#[cfg(feature = "embed")]
fn embedding_category() -> DownloadCategory {
    use crate::routes::rag::models::EmbedderConfig;
    use crate::routes::rag::registry::RagRegistry;
    use morpheus_recall::embed::{LocalEmbedModel, local_model_installed};

    let mut models = Vec::new();
    for db in RagRegistry::load().dbs {
        if let EmbedderConfig::Local { model } = db.embedder
            && !models.contains(&model)
        {
            models.push(model);
        }
    }
    if models.is_empty() {
        models.push(LocalEmbedModel::AllMiniLML6V2);
    }
    DownloadCategory {
        title: "Embedding",
        description: "Encode text as vectors for similarity search",
        models: models
            .into_iter()
            .map(|model| DownloadModel {
                label: model.label(),
                detail: String::new(),
                installed: local_model_installed(model.to_fastembed_model()),
                kind: DownloadKind::Embedding(model),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;

    #[test]
    fn selected_category_is_visible_and_clickable_in_a_short_view() {
        let mut view = DownloadView::new();
        view.categories = ["Text-to-speech", "Decision", "Embedding"]
            .into_iter()
            .map(|title| DownloadCategory {
                title,
                description: "Local models for the selected type",
                models: Vec::new(),
            })
            .collect();
        view.select_prev();
        let area = Rect::new(2, 1, 52, 4);
        let mut buf = Buffer::empty(area);
        let registry = ThemeRegistry::new();
        let theme = registry.default_theme();
        view.render(&mut buf, area, theme);

        let (x, y) = (area.y..area.bottom())
            .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
            .find(|&(x, y)| {
                let cell = &buf[(x, y)];
                cell.symbol() == "E" && cell.fg == rgba_color(theme.primary)
            })
            .expect("the selected Embedding row is visible");
        assert_eq!(
            buf[(x - SECTION_PAD - SECTION_ITEM_INDENT, y)].symbol(),
            "│"
        );
        assert_eq!(buf[(x - 2, y)].symbol(), " ");
        assert_eq!(buf[(x - 1, y)].symbol(), " ");
        assert_eq!(view.hit_test(x, y, area), MouseHit::Row(2));
        assert_eq!(view.hit_test(x, y - 1, area), MouseHit::Outside);
        assert!((area.x..area.right()).any(|x| buf[(x, area.bottom() - 1)].symbol() == "╯"));
    }

    #[test]
    fn progress_handles_unknown_totals_and_large_downloads() {
        assert_eq!(download_status(None), "downloading");
        assert_eq!(
            download_status(Some((2 * 1024 * 1024, 0))),
            "downloading 2 MiB"
        );
        assert_eq!(
            download_status(Some((u64::MAX, u64::MAX))),
            "downloading 100%"
        );
        assert_eq!(download_status(Some((200, 100))), "downloading 100%");
    }

    #[cfg(feature = "speak")]
    #[test]
    fn picker_keeps_the_categories_visible_around_it() {
        let mut view = DownloadView::new();
        view.categories = vec![
            text_to_speech_category(),
            DownloadCategory {
                title: "Embedding",
                description: "Encode text as vectors for similarity search",
                models: Vec::new(),
            },
        ];
        let area = Rect::new(0, 0, 120, 24);
        let route_area = Rect::new(32, 1, 88, 20);
        let registry = ThemeRegistry::new();
        let theme = registry.default_theme();

        let mut background = Buffer::empty(area);
        view.render(&mut background, route_area, theme);
        view.open_picker();

        for query in [String::new(), "x".repeat(48)] {
            view.picker().unwrap().filter = query;
            let count = view.matching_count();
            let layout = picker_layout(view.picker().unwrap(), count, area).unwrap();
            let mut frame = Buffer::empty(area);
            view.render(&mut frame, route_area, theme);
            view.render_picker(&mut frame, area, theme, SystemTime::UNIX_EPOCH);

            let (header_x, header_y, _) = layout.header();
            let expected_title = "Text-to-speech models";
            let title: String = (header_x..header_x + expected_title.len() as u16)
                .map(|x| frame[(x, header_y)].symbol())
                .collect();
            assert_eq!(title, expected_title);
            let overlay = layout.rect();
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    if !overlay.contains((x, y).into()) {
                        assert_eq!(frame[(x, y)], background[(x, y)], "cell at {x}, {y}");
                    }
                }
            }
        }
    }

    #[cfg(feature = "speak")]
    #[test]
    fn picker_filters_and_hit_tests_through_the_shared_layout() {
        let mut view = DownloadView::new();
        view.categories = vec![text_to_speech_category()];
        view.open_picker();
        let picker = view.picker().unwrap();
        for ch in "portuguese".chars() {
            picker.push_char(ch);
        }
        assert_eq!(view.selected_model().unwrap().label, "Kokoro v1.0");

        let area = Rect::new(5, 2, 60, 8);
        let layout = picker_layout(view.picker().unwrap(), 1, area).unwrap();
        assert_eq!(
            view.hit_test(layout.x + 1, layout.list_top(), area),
            MouseHit::Row(0)
        );
        view.picker().unwrap().push_char('z');
        assert!(view.selected_model().is_none());
        assert_eq!(
            view.hit_test(layout.x + 1, layout.list_top(), area),
            MouseHit::Inside
        );
        assert!(view.close_picker());
        view.open_picker();
        assert!(view.picker().unwrap().filter.is_empty());
    }
}
