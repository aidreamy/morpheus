use gpui::{App, ClickEvent, FontWeight, Pixels, ScrollHandle, Window, div, prelude::*, px, rgb};

pub(crate) use morpheus::commands::Command;

use crate::theme;

pub(crate) const COMMANDS: &[Command] = &[
    Command::Models,
    Command::Router,
    Command::Providers,
    Command::Settings,
    Command::Tools,
    Command::ToolCall,
    Command::Compact,
    Command::Export,
    Command::New,
    Command::Rename,
];

pub(crate) fn exact(text: &str) -> Option<Command> {
    let name = text.trim().strip_prefix('/')?;
    COMMANDS
        .iter()
        .copied()
        .find(|command| command.metadata().0 == name)
}

#[derive(Default)]
pub(crate) struct Menu {
    forced: bool,
    dismissed: bool,
    selected: usize,
    scroll: ScrollHandle,
}

impl Menu {
    pub(crate) fn input_changed(&mut self) {
        self.forced = false;
        self.dismissed = false;
        self.selected = 0;
        self.scroll.scroll_to_item(0);
    }

    pub(crate) fn toggle(&mut self, text: &str) {
        if !self.dismiss(text) {
            self.input_changed();
            self.forced = true;
        }
    }

    pub(crate) fn dismiss(&mut self, text: &str) -> bool {
        let open = !self.options(text).is_empty();
        if open {
            self.dismissed = true;
            self.forced = false;
        }
        open
    }

    pub(crate) fn options(&self, text: &str) -> Vec<Command> {
        if self.dismissed {
            return Vec::new();
        }
        let query = if self.forced {
            ""
        } else if let Some(query) = text.trim_start().strip_prefix('/') {
            query
        } else {
            return Vec::new();
        };
        COMMANDS
            .iter()
            .copied()
            .filter(|command| command.metadata().0.starts_with(query))
            .collect()
    }

    pub(crate) fn selected(&self, text: &str) -> Option<Command> {
        self.options(text).get(self.selected).copied()
    }

    pub(crate) fn move_selection(&mut self, text: &str, down: bool) -> bool {
        let count = self.options(text).len();
        if count == 0 {
            return false;
        }
        self.selected = (self.selected + if down { 1 } else { count - 1 }) % count;
        self.scroll.scroll_to_item(self.selected);
        true
    }

    pub(crate) fn render<F>(
        &self,
        text: &str,
        height: Pixels,
        on_pick: F,
    ) -> Option<impl IntoElement>
    where
        F: Fn(Command, &ClickEvent, &mut Window, &mut App) + Clone + 'static,
    {
        let options = self.options(text);
        if options.is_empty() {
            return None;
        }

        Some(
            div()
                .id("slash-menu")
                .flex()
                .flex_col()
                .min_h_0()
                .w_full()
                .mb(px(8.))
                .rounded(px(14.))
                .border_1()
                .border_color(rgb(theme::HOVER))
                .bg(rgb(theme::CHROME))
                .shadow_md()
                .block_mouse_except_scroll()
                .child(
                    div()
                        .id("command-options")
                        .min_h_0()
                        .max_h(height)
                        .p(px(5.))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(options.into_iter().enumerate().map(|(index, command)| {
                            let (name, description, _) = command.metadata();
                            let shortcut =
                                (command == Command::New).then_some(if cfg!(target_os = "macos") {
                                    "⌘T"
                                } else {
                                    "Ctrl+T"
                                });
                            let on_pick = on_pick.clone();
                            div()
                                .id(("slash-command", index))
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .px(px(10.))
                                .py(px(6.))
                                .rounded(px(8.))
                                .cursor_pointer()
                                .when(index == self.selected, |row| row.bg(rgb(theme::ACTIVE)))
                                .hover(|style| style.bg(rgb(theme::HOVER)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_size(px(14.))
                                                .line_height(px(19.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(rgb(theme::TEXT))
                                                .child(format!("/{name}")),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(12.))
                                                .line_height(px(16.))
                                                .text_color(rgb(theme::MUTED))
                                                .child(description),
                                        ),
                                )
                                .children(shortcut.map(|shortcut| {
                                    div()
                                        .flex_shrink_0()
                                        .px(px(6.))
                                        .py(px(2.))
                                        .rounded(px(5.))
                                        .border_1()
                                        .border_color(rgb(theme::FOCUS))
                                        .text_size(px(11.))
                                        .text_color(rgb(theme::TEXT))
                                        .child(shortcut)
                                }))
                                .on_click(move |event, window, cx| {
                                    on_pick(command, event, window, cx)
                                })
                        })),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_menu_offers_terminal_commands_with_desktop_handlers() {
        assert!(
            COMMANDS
                .iter()
                .all(|command| Command::ALL.contains(command))
        );
        assert_eq!(exact("/router"), Some(Command::Router));
        assert_eq!(exact("/rename"), Some(Command::Rename));
        for text in ["/close", "/stop", "/sidebar", "/right-panel", "/wallpaper"] {
            assert_eq!(exact(text), None);
            assert!(Menu::default().options(text).is_empty());
        }
    }
}
