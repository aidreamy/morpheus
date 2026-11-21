use morpheus::harness::context::ContextManager;

use super::super::speech_targets;
use crate::types::{Message, MessageRole, Part, TextPart};

fn message(id: &str, role: MessageRole, text: &str) -> Message {
    Message {
        id: id.into(),
        role,
        parts: vec![Part::Text(TextPart {
            text: text.into(),
            synthetic: false,
        })],
        created_at: 0,
        agent: None,
        model: None,
    }
}

/// Two closed loops. The first closure carries a stored audio; the second was
/// just closed and has none yet.
fn two_turns() -> (morpheus::harness::ContextManagerState, Vec<Message>) {
    let mut manager = ContextManager::new(10_000);
    manager.add_user("first question");
    manager.add_assistant("first answer", true);
    manager.close_loop();
    let mut state = manager.save_state();
    let (id, _, _) = state.last_closure_audio().expect("first closure");
    assert!(state.set_closure_audio(id, "/tmp/first.wav".into()));
    manager.restore_state(&state);

    manager.add_user("second question");
    manager.add_assistant("second answer", true);
    manager.close_loop();

    let messages = vec![
        message("u1", MessageRole::User, "first question"),
        message("a1", MessageRole::Assistant, "first answer"),
        message("u2", MessageRole::User, "second question"),
        message("a2", MessageRole::Assistant, "second answer"),
    ];
    (manager.save_state(), messages)
}

#[test]
fn an_earlier_answer_keeps_its_audio_after_a_new_turn() {
    let (state, messages) = two_turns();
    assert_eq!(state.audible_closures().len(), 1);

    let targets = speech_targets(&messages, Some(&state));

    assert_eq!(targets.len(), 2, "one target per finalized answer");
    assert_eq!(targets[0].slot, 1);
    assert_eq!(
        targets[0].audio.as_deref(),
        Some(std::path::Path::new("/tmp/first.wav")),
        "the first answer keeps its stored file"
    );
    assert_eq!(targets[1].slot, 3);
    assert!(targets[1].audio.is_none());
    assert!(
        targets[1].closure_id.is_some(),
        "the tail can be generated on demand and linked to its closure"
    );
}

#[test]
fn a_tail_without_a_context_closure_still_gets_controls() {
    let messages = vec![message("a1", MessageRole::Assistant, "the answer")];

    let targets = speech_targets(&messages, None);

    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].slot, 0);
    assert_eq!(targets[0].closure_id, None);
    assert!(targets[0].audio.is_none());
}
