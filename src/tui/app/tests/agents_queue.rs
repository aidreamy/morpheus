//! The USER's queue for `/agents` panes - the pane-level counterpart of the
//! main agent's `PendingQueues`.
//!
//! The pane never had one: a submission aimed at a busy pane was refused with
//! a toast, while an AGENT could already queue work for that same pane
//! through `agents_call`. These tests pin the two user-facing queues on BOTH
//! pane kinds (a morpheus harness is steered mid-turn, an ACP agent takes its
//! next request at the prompt boundary), their degradation rules and the
//! settling that keeps a message from being stranded when the turn it was
//! meant to join ends first.

use super::{App, HOME_LOCK, isolate_home, mod_key};
use crate::routes::session::agents_split::AgentsSplitState;
use crate::routes::session::queue_choice::QueueTarget;
use crossterm::event::{KeyCode, KeyModifiers};

fn app_with_panes(agents: &[&str]) -> App {
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.agents_split = Some(AgentsSplitState::new(
        agents.iter().map(|a| (*a).into()).collect(),
    ));
    app
}

/// Focus pane `pane` (the `Alt+<n>` hotkey, 1-based) and submit `text`.
fn focus_and_submit(app: &mut App, pane: usize, text: &str) {
    app.process_key_event(mod_key(
        KeyCode::Char(char::from(b'1' + pane as u8)),
        KeyModifiers::ALT,
    ))
    .unwrap();
    app.prompt_view.input = text.to_string();
    app.process_key_event(mod_key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
}

fn queued(app: &App, pane: usize) -> (Vec<String>, Vec<String>) {
    let pane = &app.agents_split.as_ref().unwrap().panes[pane];
    (
        pane.queued.next_request.iter().cloned().collect(),
        pane.queued.next_loop.iter().cloned().collect(),
    )
}

/// A busy ACP pane offers BOTH queues - its next request goes out when the
/// running prompt completes, so the control is honorable - and the message is
/// QUEUED for that pane instead of being refused.
#[tokio::test]
async fn enter_on_a_busy_acp_pane_offers_both_queues() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_panes(&["claude"]);
    let pane_id = app.agents_split.as_ref().unwrap().panes[0].id;
    app.agents_split.as_mut().unwrap().set_running(0, true);

    focus_and_submit(&mut app, 0, "keep going");

    assert!(
        app.queue_choice_dialog.visible,
        "a busy pane must ask which of ITS queues the message joins"
    );
    assert_eq!(
        app.queue_choice_dialog.pane(),
        Some(pane_id),
        "the choice names the pane, by stable id"
    );
    assert_eq!(
        app.prompt_view.input, "keep going",
        "the draft stays until the choice is confirmed"
    );
    // Both rows exist: `2` reaches the loop row, `1` returns to the request
    // row - an ACP pane is not a second-class queue target.
    assert_eq!(app.queue_choice_dialog.choice(), QueueTarget::NextRequest);
    app.queue_choice_dialog
        .handle_key_event(mod_key(KeyCode::Char('2'), KeyModifiers::NONE));
    assert_eq!(app.queue_choice_dialog.choice(), QueueTarget::NextLoop);
    app.queue_choice_dialog
        .handle_key_event(mod_key(KeyCode::Char('1'), KeyModifiers::NONE));
    assert_eq!(app.queue_choice_dialog.choice(), QueueTarget::NextRequest);

    // Confirm with the Enter key: the message lands in THAT pane's queue.
    app.process_key_event(mod_key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let (next_request, next_loop) = queued(&app, 0);
    assert_eq!(next_request, vec!["keep going".to_string()]);
    assert!(next_loop.is_empty());
    assert!(
        app.prompt_view.input.is_empty(),
        "the message left the prompt"
    );
    assert!(
        app.agents_split.as_ref().unwrap().panes[0].running,
        "the pane's own turn was untouched"
    );
    assert!(
        app.state
            .current_session_id
            .as_ref()
            .and_then(|id| app.state.pending_queues.get(id))
            .is_none_or(|q| q.queued_count() == 0),
        "the MAIN agent's queues must not receive a pane message"
    );
}

/// A morpheus pane has request boundaries inside its turn, so the \"next
/// request\" row IS offered, and the pump hands the message to the live
/// loop's steering queue (echoing it into the pane's timeline).
#[tokio::test]
async fn a_morpheus_pane_mid_turn_takes_the_next_request_queue() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_panes(&["morpheus"]);
    let steer = morpheus::harness::queued_input::QueuedInput::default();
    {
        let split = app.agents_split.as_mut().unwrap();
        split.set_running(0, true);
        split.set_steer(0, Some(steer.clone()));
    }

    focus_and_submit(&mut app, 0, "steer this");

    assert!(app.queue_choice_dialog.visible);
    assert_eq!(
        app.queue_choice_dialog.choice(),
        QueueTarget::NextRequest,
        "the steerable pane defaults to the next-request queue"
    );
    app.process_key_event(mod_key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let (next_request, next_loop) = queued(&app, 0);
    assert_eq!(next_request, vec!["steer this".to_string()]);
    assert!(next_loop.is_empty());

    // The frame pump feeds the live loop one message at a time.
    app.pump_agents_pane_queued_requests();
    let (next_request, _) = queued(&app, 0);
    assert!(
        next_request.is_empty(),
        "the delivered message leaves the pane's queue"
    );
    assert_eq!(
        steer.drain(),
        vec!["steer this".to_string()],
        "the running harness loop received it for its next request"
    );
}

/// A \"next request\" pick on a pane with no live loop degrades to the next
/// loop: a queue nothing would drain must never hold the message.
#[tokio::test]
async fn a_next_request_pick_on_an_idle_pane_becomes_the_next_loop() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_panes(&["morpheus"]);
    let pane_id = app.agents_split.as_ref().unwrap().panes[0].id;

    app.enqueue_pane_message(pane_id, QueueTarget::NextRequest, "later".into());

    let (next_request, next_loop) = queued(&app, 0);
    assert!(next_request.is_empty(), "no loop, no steering slot");
    assert_eq!(next_loop, vec!["later".to_string()]);
}

/// Settling leftovers: messages the finished turn never drained (they were
/// still in its steering handle) go back to the head-of-line `next request`
/// queue, in FIFO order, behind whatever was already waiting. Nothing is
/// stranded between the turn ending and the handle being dropped, and the
/// messages keep their standing for the moment the pane takes its next
/// request.
#[tokio::test]
async fn leftovers_return_to_the_next_request_queue() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_panes(&["morpheus"]);
    let steer = morpheus::harness::queued_input::QueuedInput::default();
    {
        let split = app.agents_split.as_mut().unwrap();
        split.panes[0].queued.next_request.push_back("a".into());
        split.panes[0].queued.next_request.push_back("b".into());
        steer.push("left in the handle".into());
        split.set_steer(0, Some(steer));
        // A cold-restore replay is in flight: the pane is not running, but
        // it must not take a turn yet either.
        split.panes[0].rehydrating = true;
    }

    app.serve_agents_pane_user_queues();

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert_eq!(
        pane.queued.next_request.iter().cloned().collect::<Vec<_>>(),
        vec![
            "a".to_string(),
            "b".to_string(),
            "left in the handle".to_string()
        ],
        "the drained leftovers rejoin the queue in FIFO order, losing nothing"
    );
    assert!(
        pane.queued.next_loop.is_empty(),
        "they keep their standing: they were meant as requests"
    );
    assert!(pane.steer.is_none(), "the dead handle is dropped");
    assert!(
        !pane.running,
        "a rehydrating pane waits for its history before taking a turn"
    );
}

/// An ACP pane's turn is ONE prompt, so its "next request" message cannot be
/// injected mid-turn - it goes out at the prompt boundary, consumed by the
/// queue service the moment that prompt completes, so the pane never sits
/// idle between the two. This is the pane-level twin of the morpheus harness
/// draining its steering handle before the next request.
#[tokio::test]
async fn an_acp_panes_next_request_rides_out_at_the_turn_boundary() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_panes(&["claude"]);
    let pane_id = app.agents_split.as_ref().unwrap().panes[0].id;
    app.agents_split.as_mut().unwrap().set_running(0, true);
    app.enqueue_pane_message(pane_id, QueueTarget::NextRequest, "and then".into());

    // While the prompt runs the message HOLDS: there is no request boundary
    // inside an ACP turn to inject at.
    app.pump_agents_pane_queued_requests();
    assert_eq!(queued(&app, 0).0, vec!["and then".to_string()]);

    // The turn completes: the pane is free for exactly one pass of the queue
    // service, which turns the message into its next request right away.
    app.agents_split.as_mut().unwrap().set_running(0, false);
    app.serve_agents_pane_user_queues();

    let (next_request, next_loop) = queued(&app, 0);
    assert!(
        next_request.is_empty() && next_loop.is_empty(),
        "the delivery is the turn's own work now, not a queue entry"
    );
    assert!(
        app.agents_split.as_ref().unwrap().panes[0].running,
        "the pane took its next request without ever sitting idle"
    );
    let echoed: Vec<&str> = app.agents_split.as_ref().unwrap().panes[0]
        .activity
        .timeline
        .iter()
        .filter_map(|entry| match entry {
            crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage {
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        echoed,
        vec!["and then"],
        "the pane shows the message as its own turn's input"
    );
}

/// The two queues keep the main agent's priority on a pane too: the message
/// picked as the pane's next REQUEST goes first, and a next-loop message
/// waits for the following turn.
#[tokio::test]
async fn a_next_request_outranks_the_next_loop_when_the_pane_frees() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_panes(&["claude"]);
    {
        let pane = &mut app.agents_split.as_mut().unwrap().panes[0];
        pane.queued.next_loop.push_back("loop".into());
        pane.queued.next_request.push_back("request".into());
    }

    app.serve_agents_pane_user_queues();

    let (next_request, next_loop) = queued(&app, 0);
    assert!(next_request.is_empty(), "the request went out");
    assert_eq!(
        next_loop,
        vec!["loop".to_string()],
        "the loop message waits for the next turn"
    );
    assert!(app.agents_split.as_ref().unwrap().panes[0].running);
}

/// The "next agent loop" queue starts the pane's next turn once it is free -
/// the delivery a pane that was busy, compacting or still replaying its
/// history gets, for BOTH pane kinds.
#[tokio::test]
async fn a_next_loop_message_starts_the_pane_turn_when_it_frees() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_panes(&["claude"]);
    app.agents_split.as_mut().unwrap().panes[0]
        .queued
        .next_loop
        .push_back("after the loop".into());

    app.serve_agents_pane_user_queues();

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert!(
        pane.queued.is_empty(),
        "the queued message became the pane's turn"
    );
    assert!(pane.running, "the pane's next loop started");
}

/// A stop (Ctrl+X) ends the steering handle with the turn it fed: the
/// messages still sitting in it are the user's - they go back to the pane's
/// "next request" queue and start its next turn, instead of being dropped
/// with the dead handle (the stopped turn's terminal Done is refused, so
/// nothing else recovers them).
#[tokio::test]
async fn a_stop_recovers_the_messages_left_in_the_steering_handle() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_panes(&["claude"]);
    app.process_key_event(mod_key(KeyCode::Char('1'), KeyModifiers::ALT))
        .unwrap();
    let steer = morpheus::harness::queued_input::QueuedInput::default();
    steer.push("picked for the next request".into());
    {
        let split = app.agents_split.as_mut().unwrap();
        split.set_running(0, true);
        split.set_stop_signal(
            0,
            Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            ))),
        );
        split.begin_turn(0);
        split.set_steer(0, Some(steer.clone()));
    }

    app.process_key_event(mod_key(KeyCode::Char('x'), KeyModifiers::CONTROL))
        .unwrap();

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert!(steer.is_empty(), "the dead handle was drained");
    assert!(pane.steer.is_none(), "and dropped with the turn");
    assert!(
        pane.queued.is_empty(),
        "nothing is stranded: the message became the pane's next turn"
    );
    assert!(pane.running, "the recovered message started that turn");
}

/// Esc leaves the draft unsent and queues nothing - the dialog contract the
/// main agent's flow already had.
#[tokio::test]
async fn esc_on_the_pane_queue_dialog_keeps_the_draft() {
    let _home = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_panes(&["claude"]);
    app.agents_split.as_mut().unwrap().set_running(0, true);

    focus_and_submit(&mut app, 0, "hold on");
    assert!(app.queue_choice_dialog.visible);

    app.process_key_event(mod_key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();

    assert!(!app.queue_choice_dialog.visible);
    assert_eq!(app.prompt_view.input, "hold on");
    let (next_request, next_loop) = queued(&app, 0);
    assert!(next_request.is_empty() && next_loop.is_empty());
}
