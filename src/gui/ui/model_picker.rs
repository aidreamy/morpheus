use gpui::{
    App, ClickEvent, FontWeight, Pixels, ScrollStrategy, UniformListScrollHandle, Window, div,
    prelude::*, px, rgb, uniform_list,
};
use morpheus::{ModelEntry, models::matches_filter};

use crate::theme;

const ROW_HEIGHT: Pixels = px(44.);

pub(crate) struct ModelPicker {
    models: Vec<ModelEntry>,
    current: Option<ModelEntry>,
    selected: usize,
    loading: bool,
    scroll: UniformListScrollHandle,
}

impl Default for ModelPicker {
    fn default() -> Self {
        Self {
            models: vec![auto_model()],
            current: None,
            selected: 0,
            loading: false,
            scroll: UniformListScrollHandle::new(),
        }
    }
}

impl ModelPicker {
    pub(crate) fn open(&mut self, current: Option<ModelEntry>) -> bool {
        self.current = current;
        self.select_current("");
        !std::mem::replace(&mut self.loading, true)
    }

    pub(crate) fn loaded(&mut self, models: Vec<ModelEntry>, query: &str) {
        let previously_selected = self.selected(query).cloned();
        let first_load = self.models.len() == 1 && self.selected == 0;
        self.loading = false;
        self.models.truncate(1);
        self.models.extend(models);
        if first_load && query.is_empty() {
            self.select_current(query);
        } else {
            let index = self.index_of(previously_selected.as_ref(), query);
            self.select(index);
        }
    }

    fn matches<'a>(&'a self, query: &'a str) -> impl Iterator<Item = &'a ModelEntry> + 'a {
        self.models
            .iter()
            .filter(move |entry| matches_filter(entry, query.trim()))
    }

    fn index_of(&self, model: Option<&ModelEntry>, query: &str) -> usize {
        model
            .and_then(|model| {
                self.matches(query)
                    .position(|entry| same_model(entry, model))
            })
            .unwrap_or(0)
    }

    fn select_current(&mut self, query: &str) {
        let index = self.index_of(self.current.as_ref(), query);
        self.select(index);
    }

    pub(crate) fn select(&mut self, index: usize) {
        self.selected = index;
        self.scroll.scroll_to_item(index, ScrollStrategy::Top);
    }

    pub(crate) fn selected<'a>(&'a self, query: &'a str) -> Option<&'a ModelEntry> {
        self.matches(query).nth(self.selected)
    }

    pub(crate) fn navigate(&mut self, query: &str, down: bool) {
        let count = self.matches(query).count();
        if count > 0 {
            self.selected = (self.selected + if down { 1 } else { count - 1 }) % count;
            self.scroll
                .scroll_to_item(self.selected, ScrollStrategy::Top);
        }
    }

    pub(crate) fn render<F>(
        &self,
        query: &str,
        input: impl IntoElement,
        height: Pixels,
        on_pick: F,
    ) -> impl IntoElement
    where
        F: Fn(ModelEntry, &ClickEvent, &mut Window, &mut App) + Clone + 'static,
    {
        let choices: Vec<_> = self.matches(query).cloned().collect();
        let selected = self.selected;
        let current = self.current.clone();
        let status = if self.loading {
            "Loading models…"
        } else if choices.is_empty() {
            "No models match this search."
        } else if self.models.len() == 1 {
            "No provider models are available."
        } else {
            "↑ ↓ to navigate · Enter to select"
        };
        let list_height = (ROW_HEIGHT * choices.len()).min(height.max(ROW_HEIGHT));

        div()
            .id("model-picker")
            .flex()
            .flex_col()
            .w_full()
            .min_h_0()
            .min_w_0()
            .p(px(10.))
            .rounded(px(14.))
            .bg(rgb(theme::CHROME))
            .border_1()
            .border_color(rgb(theme::HOVER))
            .shadow_md()
            .block_mouse_except_scroll()
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .justify_between()
                    .items_center()
                    .px(px(3.))
                    .mb(px(8.))
                    .text_color(rgb(theme::TEXT))
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Choose a model")
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(theme::MUTED))
                            .child("Esc"),
                    ),
            )
            .child(input)
            .when(!choices.is_empty(), |panel| {
                panel.child(
                    uniform_list("available-models", choices.len(), move |range, _, _| {
                        range
                            .map(|index| {
                                let entry = choices[index].clone();
                                let on_pick = on_pick.clone();
                                let active = current
                                    .as_ref()
                                    .is_some_and(|current| same_model(current, &entry));
                                let provider = if entry.provider.is_empty() {
                                    "Fallback routing".to_owned()
                                } else {
                                    entry.provider.clone()
                                };
                                div()
                                    .id(index)
                                    .w_full()
                                    .h(ROW_HEIGHT)
                                    .px(px(9.))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .cursor_pointer()
                                    .rounded(px(7.))
                                    .hover(|style| style.bg(rgb(theme::HOVER)))
                                    .when(index == selected, |row| row.bg(rgb(theme::ACTIVE)))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(13.))
                                                    .line_height(px(18.))
                                                    .text_color(rgb(theme::TEXT))
                                                    .child(entry.model.clone()),
                                            )
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(11.))
                                                    .line_height(px(15.))
                                                    .text_color(rgb(theme::MUTED))
                                                    .child(provider),
                                            ),
                                    )
                                    .when(active, |row| {
                                        row.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(px(11.))
                                                .text_color(rgb(theme::MUTED))
                                                .child("Current"),
                                        )
                                    })
                                    .on_click(move |event, window, cx| {
                                        on_pick(entry.clone(), event, window, cx)
                                    })
                            })
                            .collect()
                    })
                    .mt(px(6.))
                    .w_full()
                    .min_h(ROW_HEIGHT)
                    .h(list_height)
                    .track_scroll(self.scroll.clone()),
                )
            })
            .child(
                div()
                    .flex_shrink_0()
                    .pt(px(8.))
                    .px(px(3.))
                    .text_size(px(11.))
                    .text_color(rgb(theme::MUTED))
                    .child(status),
            )
    }
}

fn same_model(left: &ModelEntry, right: &ModelEntry) -> bool {
    left.provider == right.provider && left.model == right.model
}

fn auto_model() -> ModelEntry {
    ModelEntry {
        provider: String::new(),
        model: "auto".into(),
    }
}
