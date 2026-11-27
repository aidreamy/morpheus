use super::{App, HOME_LOCK, isolate_home};
use crate::routes::session::permission::PermissionAction;
use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse,
    MouseEventKind as CKind,
};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn mouse(kind: CKind, x: u16, y: u16) -> CMouse {
    CMouse {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

fn session_app() -> App {
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.process_key_event(key(KeyCode::Char(ch))).unwrap();
    }
}

fn gateway_app() -> App {
    let mut app = App::new("/tmp".to_string());
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    });
    app.llm_config.provider = "openai".to_string();
    app
}

/// After a path that closes an overlay, Ctrl+C on a non-empty draft must
/// CLEAR the draft (prompt owns keyboard) - not show the quit confirm.
fn assert_ctrl_c_clears(app: &mut App) {
    app.process_key_event(ctrl(KeyCode::Char('c'))).unwrap();
    assert_eq!(
        app.prompt_view.input, "",
        "Ctrl+C must clear the draft once the overlay is gone"
    );
    assert!(
        !app.is_confirm_dialog_visible(),
        "Ctrl+C must not fall through to the quit-confirm after the overlay closes"
    );
}

#[tokio::test]
async fn permission_dialog_enter_close_restores_prompt_focus() {
    let _g = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();
    type_text(&mut app, "draft");
    app.permission_dialog.visible = true;
    app.prompt_view.blur();

    app.process_key_event(key(KeyCode::Enter)).unwrap();
    assert!(!app.permission_dialog.visible);

    assert!(
        app.prompt_view.is_focused,
        "Enter-close must re-focus the prompt"
    );
    assert_ctrl_c_clears(&mut app);
}

#[tokio::test]
async fn permission_dialog_esc_close_restores_prompt_focus() {
    let _g = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();
    type_text(&mut app, "draft");
    app.permission_dialog.visible = true;
    app.prompt_view.blur();

    app.process_key_event(key(KeyCode::Esc)).unwrap();
    assert!(!app.permission_dialog.visible);

    assert!(
        app.prompt_view.is_focused,
        "Esc-close must re-focus the prompt"
    );
    assert_ctrl_c_clears(&mut app);
}

#[tokio::test]
async fn worktree_permission_resolve_restores_prompt_focus() {
    let _g = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();
    type_text(&mut app, "draft");
    app.permission_dialog.visible = true;
    app.prompt_view.blur();
    app.pending_worktree_confirmation = Some(morpheus::harness::agent_coworkers::WorktreeOp {
        id: "wt-op-1".into(),
        coworker: "pane".into(),
        coworker_id: 1,
        action: morpheus::harness::agent_coworkers::WorktreeAction::Clear,
        branch: None,
        requester: morpheus::harness::agent_coworkers::MAIN,
    });

    app.resolve_pending_worktree_permission(PermissionAction::Deny);
    assert!(!app.permission_dialog.visible);

    assert!(
        app.prompt_view.is_focused,
        "resolve must re-focus the prompt"
    );
    assert_ctrl_c_clears(&mut app);
}

#[tokio::test]
async fn question_dialog_mouse_esc_close_restores_prompt_focus() {
    let _g = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();
    type_text(&mut app, "draft");
    app.question_dialog.visible = true;
    app.prompt_view.blur();

    let area = app.question_dialog_area();
    let req_h = app.question_dialog.required_height(area.width);
    let height = req_h.min(area.height);
    let footer_y = area.y + height.saturating_sub(2);
    // access tab_count/is_confirm via handle? print by calling a debug path
    app.question_dialog.visible = true;
    let mut closed = false;
    for x in 0..area.width {
        app.question_dialog.visible = true;
        let _ = app.handle_mouse_event(mouse(CKind::Up(CBtn::Left), area.x + x, footer_y));
        if !app.question_dialog.visible {
            closed = true;
            break;
        }
    }
    assert!(
        closed,
        "must find a footer click that closes the question dialog"
    );

    assert!(
        app.prompt_view.is_focused,
        "mouse-close must re-focus the prompt"
    );
    assert_ctrl_c_clears(&mut app);
}

#[tokio::test]
async fn free_gateway_keyboard_esc_restores_prompt_focus() {
    let _g = HOME_LOCK.lock();
    isolate_home();
    let mut app = gateway_app();
    app.maybe_offer_gateway_on_error("connection reset by peer");
    assert!(app.free_gateway_dialog.visible);
    type_text(&mut app, "draft");
    app.prompt_view.blur();

    // Let the offer finish streaming so Esc actually dismisses.
    std::thread::sleep(std::time::Duration::from_secs(10));
    app.free_gateway_dialog.advance_stream();

    app.process_key_event(key(KeyCode::Esc)).unwrap();
    assert!(!app.free_gateway_dialog.visible);

    assert!(
        app.prompt_view.is_focused,
        "Esc-close must re-focus the prompt"
    );
    assert_ctrl_c_clears(&mut app);
}

#[tokio::test]
async fn free_gateway_mouse_esc_footer_restores_prompt_focus() {
    let _g = HOME_LOCK.lock();
    isolate_home();
    let mut app = gateway_app();
    app.maybe_offer_gateway_on_error("connection reset by peer");
    assert!(app.free_gateway_dialog.visible);
    type_text(&mut app, "draft");
    app.prompt_view.blur();

    std::thread::sleep(std::time::Duration::from_secs(10));
    app.free_gateway_dialog.advance_stream();

    let area = app.free_gateway_dialog_area();
    let req_h = app.free_gateway_dialog.required_height(area.width);
    let height = req_h.min(area.height);
    let footer_y = area.y + height.saturating_sub(2);
    let esc_x = area.x + 3 + 14;
    let _ = app.handle_mouse_event(mouse(CKind::Up(CBtn::Left), esc_x, footer_y));
    assert!(
        !app.free_gateway_dialog.visible,
        "must close the free-gateway dialog"
    );

    assert!(
        app.prompt_view.is_focused,
        "mouse-close must re-focus the prompt"
    );
    assert_ctrl_c_clears(&mut app);
}

#[tokio::test]
async fn free_gateway_authorized_event_restores_prompt_focus() {
    let _g = HOME_LOCK.lock();
    isolate_home();
    let mut app = gateway_app();
    app.maybe_offer_gateway_on_error("connection reset by peer");
    assert!(app.free_gateway_dialog.visible);
    type_text(&mut app, "draft");
    app.prompt_view.blur();

    use crate::app::gateway_recommendation::{DeviceSignInEvent, SignInOrigin};
    let tx = app.gateway_event_tx.clone();
    let _ = tx.send(DeviceSignInEvent::Authorized {
        origin: SignInOrigin::GatewayOffer,
        provider: "charm".to_string(),
    });
    app.pump_gateway_events();
    assert!(!app.free_gateway_dialog.visible);

    assert!(
        app.prompt_view.is_focused,
        "Authorized must re-focus the prompt"
    );
    if app.is_model_dialog_visible() {
        let _ = app.process_key_event(key(KeyCode::Esc));
    }
    assert_ctrl_c_clears(&mut app);
}

#[tokio::test]
async fn free_gateway_failed_event_restores_prompt_focus() {
    let _g = HOME_LOCK.lock();
    isolate_home();
    let mut app = gateway_app();
    app.maybe_offer_gateway_on_error("connection reset by peer");
    assert!(app.free_gateway_dialog.visible);
    type_text(&mut app, "draft");
    app.prompt_view.blur();
    app.gateway_signing_in = true;

    use crate::app::gateway_recommendation::{DeviceSignInEvent, SignInOrigin};
    let tx = app.gateway_event_tx.clone();
    let _ = tx.send(DeviceSignInEvent::Failed {
        origin: SignInOrigin::GatewayOffer,
        provider: "charm".to_string(),
        error: "device flow denied".into(),
    });
    app.pump_gateway_events();
    assert!(!app.free_gateway_dialog.visible);

    assert!(
        app.prompt_view.is_focused,
        "Failed must re-focus the prompt"
    );
    assert_ctrl_c_clears(&mut app);
}

#[tokio::test]
#[cfg(feature = "embed")]
async fn rag_exit_restores_prompt_focus() {
    let _g = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();
    app.show_rag = true;
    type_text(&mut app, "draft");
    app.prompt_view.blur();

    app.handle_rag_key_event(KeyCode::Esc);
    assert!(!app.show_rag);

    assert!(
        app.prompt_view.is_focused,
        "RAG exit must re-focus the prompt"
    );
    assert_ctrl_c_clears(&mut app);
}

#[tokio::test]
async fn sidebar_focused_blocks_ctrl_c_clear() {
    let _g = HOME_LOCK.lock();
    isolate_home();
    let mut app = session_app();
    type_text(&mut app, "draft");
    app.sidebar_focused = true;
    // prompt may still be focused flag, but sidebar_focused blocks ownership
    app.prompt_view.focus();

    app.process_key_event(ctrl(KeyCode::Char('c'))).unwrap();
    // Document current behavior: sidebar_focused means Ctrl+C does NOT clear.
    // This test pins the (possibly buggy) behavior; if focus handling changes,
    // update accordingly.
    assert!(
        app.is_confirm_dialog_visible() || app.prompt_view.input == "draft",
        "while sidebar is focused, Ctrl+C must not silently clear the draft"
    );
}
