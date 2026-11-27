use super::super::{App, PendingSessionDelete};
use crossterm::event::KeyCode;

use crate::ui::dialogs::{ConfirmAction, DialogType};

/// First-run telemetry consent question. The explanatory text is deliberately
/// part of the SAME Confirm box used by quit/delete: multi-line messages make
/// the box grow (see `confirm_dialog_h`), so the user sees the full disclosure
/// before answering.
const TELEMETRY_CONSENT_MESSAGE: &str = "\
Allow telemetry collection?

Telemetry is used only to improve morpheus: aggregated
metrics about usage, features, tools, errors and token
counts. No sensitive data is collected: no conversations,
prompts, responses, file contents, paths, keys or account
data.

Audit the implementation and exactly what is collected:
https://github.com/aidreamy/morpheus/tree/main/src/telemetry
https://github.com/aidreamy/morpheus/tree/main/docs/telemetry

You can change this later in Settings.";

impl App {
    pub(in crate::app) fn is_confirm_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::Confirm { .. })
            )
    }

    /// True while the first-run consent box is the active Confirm. Its answer
    /// is persisted to `setup.json` instead of applying a destructive action.
    pub(in crate::app) fn is_telemetry_consent_visible(&self) -> bool {
        self.dialog.current().is_some_and(|d| {
            matches!(d.dialog_type, DialogType::Confirm { .. })
                && d.confirm_action == ConfirmAction::TelemetryConsent
        })
    }

    /// Ask for telemetry consent on the first run only. `telemetry_prompted`
    /// records the answer so the box never returns; the answer itself becomes
    /// `setup.telemetry` (the effective consent still passes through
    /// `Telemetry::resolve`, honoring env overrides and the CI hard-off).
    ///
    /// The preselected option mirrors the persisted setting: a user arriving
    /// with telemetry on keeps "Yes" (the historical default), and a user who
    /// already opted out keeps "No" - pressing Enter never silently flips a
    /// saved preference.
    pub fn show_telemetry_consent_prompt(&mut self) {
        if self.setup.telemetry_prompted {
            return;
        }
        self.pending_delete = None;
        self.dialog
            .show_confirm(TELEMETRY_CONSENT_MESSAGE, ConfirmAction::TelemetryConsent);
        let selected = usize::from(!self.setup.telemetry);
        if let Some(d) = self.dialog.current_mut() {
            d.selected = selected;
        }
    }

    /// Persist the consent answer and apply it to the live facade immediately
    /// (the session that is starting was resolved before the question).
    pub(in crate::app) fn resolve_telemetry_consent(&mut self, allowed: bool) {
        self.setup.telemetry = allowed;
        self.setup.telemetry_prompted = true;
        self.setup.save();
        self.telemetry = morpheus::telemetry::Telemetry::resolve(self.setup.telemetry);
        self.dialog.pop();
    }

    pub(in crate::app) fn handle_confirm_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_confirm_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Left | KeyCode::Right => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected ^= 1;
                }
                true
            }
            KeyCode::Enter => {
                self.resolve_confirm_dialog();
                true
            }
            KeyCode::Esc => {
                // Dismissing the consent box is treated as declining: the box
                // asks permission, so silence must not be read as "Yes".
                if self.is_telemetry_consent_visible() {
                    self.resolve_telemetry_consent(false);
                    return true;
                }
                self.pending_delete = None;
                self.clear_rag_pending_state();
                self.dialog.pop();
                true
            }
            // Sovereign modal: every other key is swallowed while the user
            // is deciding - nothing else in the TUI may react to it.
            _ => true,
        }
    }

    /// Apply the currently selected Confirm option: "Yes" resolves the
    /// pending destructive action (session delete, RAG delete, quit);
    /// "No" cancels. Mirrors the keymap's generic Confirm path.
    ///
    /// The `agents_worktree` confirmation no longer lives here: it renders
    /// in the SESSION permission box (`routes::session::permission`) and
    /// resolves through `App::resolve_pending_worktree_permission`
    /// (keys.rs / mouse.rs), so this dialog only handles the destructive
    /// actions again.
    fn resolve_confirm_dialog(&mut self) {
        let (selected, action) = self
            .dialog
            .current()
            .map(|d| (d.selected, d.confirm_action))
            .unwrap_or((1, ConfirmAction::Generic));
        if action == ConfirmAction::TelemetryConsent {
            self.resolve_telemetry_consent(selected == 0);
            return;
        }
        if selected == 0 {
            if self.resolve_forget_key_confirmation() {
                return;
            }
            if let Some(pending) = self.pending_delete.take() {
                match pending {
                    PendingSessionDelete::One(session_id) => {
                        self.delete_session(&session_id);
                        self.dialog.pop();
                    }
                    PendingSessionDelete::AllOfCwd => {
                        self.delete_all_sessions();
                        self.dialog.pop();
                    }
                }
            } else if self.handle_rag_confirm_delete() {
                // handled (pops the dialog itself)
            } else {
                self.should_quit = true;
            }
        } else {
            self.pending_delete = None;
            self.clear_rag_pending_state();
            self.dialog.pop();
        }
    }
}
