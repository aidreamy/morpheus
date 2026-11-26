use gpui::{App, KeyBinding, actions};

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
        ChooseModel,
        Cancel,
        StopTurn
    ]
);

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
    ]);
}
