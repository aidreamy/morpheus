use gpui::{App, ClickEvent, FontWeight, Pixels, ScrollHandle, Window, div, prelude::*, px, rgb};

pub(crate) use morpheus::commands::Command;

use crate::theme;

pub(crate) fn invocation(text: &str) -> Option<(Command, &str)> {
    let command = text.trim().strip_prefix('/')?;
    if command.contains(['\n', '\r']) {
        return None;
    }
    let (name, args) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    Some((
        Command::from_name(&name.to_ascii_lowercase())?,
        args.trim_start(),
    ))
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
        } else if let Some(query) = text.trim().strip_prefix('/') {
            if let Some((command @ (Command::Models | Command::Worktree), args)) = invocation(text)
                && !args.is_empty()
            {
                return vec![command];
            }
            query
        } else {
            return Vec::new();
        };
        let query = query.to_ascii_lowercase();
        Command::ALL
            .iter()
            .copied()
            .filter(|command| {
                crate::app::offers_command(*command) && command.metadata().0.contains(&query)
            })
            .collect()
    }

    fn selected(&self, text: &str) -> Option<Command> {
        self.options(text).get(self.selected).copied()
    }

    pub(crate) fn submit<'a>(&self, text: &'a str) -> Option<(Command, &'a str)> {
        if self.forced {
            self.selected(text).map(|command| (command, ""))
        } else {
            invocation(text).or_else(|| self.selected(text).map(|command| (command, "")))
        }
    }

    pub(crate) fn consumes_draft(&self) -> bool {
        !self.forced
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
        let menu = Menu::default();
        assert_eq!(menu.options("/rename"), [Command::Rename]);
        assert_eq!(menu.options("/toolcall"), [Command::ToolCall]);
        assert_eq!(menu.options("/CALL"), [Command::ToolCall]);
        assert_eq!(menu.options("/worktree"), [Command::Worktree]);
        assert_eq!(menu.options("/worktree list"), [Command::Worktree]);
        for text in [
            "/close",
            "/stop",
            "/sidebar",
            "/right-panel",
            "/wallpaper",
            "/agents",
        ] {
            assert!(menu.options(text).is_empty());
        }
    }

    #[test]
    fn complete_commands_and_arguments_resolve_after_dismissing_the_menu() {
        let mut menu = Menu::default();
        assert!(menu.dismiss("/models"));
        assert_eq!(menu.submit("/models "), Some((Command::Models, "")));
        assert_eq!(
            menu.submit(" /models  auto "),
            Some((Command::Models, "auto"))
        );
        assert_eq!(
            menu.submit(r#"/worktree add "/tmp/my tree" branch"#),
            Some((Command::Worktree, r#"add "/tmp/my tree" branch"#)),
        );
        assert_eq!(
            menu.submit("/wallpaper off"),
            Some((Command::Wallpaper, "off"))
        );
        assert_eq!(menu.submit("/tmp/my-tree"), None);
        assert_eq!(menu.submit("/models\nDocument these commands."), None);
    }

    #[test]
    fn command_palette_preserves_the_current_draft() {
        let mut menu = Menu::default();
        menu.toggle("/tmp/path");
        assert_eq!(menu.submit("/tmp/path"), Some((Command::Models, "")));
        assert!(!menu.consumes_draft());
    }
}
