use gpui::{Action, App, KeyBinding, actions};
use morpheus::{ModelEntry, commands::Command};

actions!(
    desktop,
    [
        NewTab,
        CloseTab,
        NextTab,
        PreviousTab,
        ToggleSidebar,
        ToggleRightPanel,
        ToggleCommands,
        Cancel
    ]
);

#[derive(Clone, PartialEq, Action)]
#[action(namespace = desktop, no_json)]
pub(crate) struct RunCommand(pub(crate) Command, pub(crate) String);

#[derive(Clone, Action)]
#[action(namespace = desktop, no_json)]
pub(crate) struct ChooseModel(pub(crate) ModelEntry);

impl PartialEq for ChooseModel {
    fn eq(&self, other: &Self) -> bool {
        self.0.provider == other.0.provider && self.0.model == other.0.model
    }
}

pub(crate) fn bind(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-t", NewTab, Some("Desktop")),
        KeyBinding::new("secondary-w", CloseTab, Some("Desktop")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Desktop")),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("Desktop")),
        KeyBinding::new("ctrl-b", ToggleSidebar, Some("Desktop")),
        KeyBinding::new("ctrl-p", ToggleRightPanel, Some("Desktop")),
        KeyBinding::new("ctrl-k", ToggleCommands, Some("Desktop")),
        KeyBinding::new("escape", Cancel, Some("Desktop")),
        KeyBinding::new("escape", Cancel, Some("CommandDialog")),
    ]);
}
