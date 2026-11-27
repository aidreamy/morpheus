# TODO — `src/speak` integration into the TUI

Original plan (kept for reference) and final state. Every item was reviewed at
the end of the implementation; whatever was not done is stated as such, with
the reason.

## 1. TTS daemon (`src/daemon/tts`)

- [x] `protocol.rs`: `NAME = "tts"`, `PROTOCOL = 1`, methods `hello`,
      `tts/synthesize`, `tts/health`, `shutdown`; typed params/results.
- [x] `server.rs`: bind/serve following the `decision` daemon (per-connection
      accept, mandatory handshake, single synthesis slot, drain, idle-exit).
- [x] `client.rs`: `TtsClient` over the shared `DaemonClient`
      (`call_with_budget`), fail-open (errors become `Err` for the caller).
- [x] `main.rs` + `[[bin]] morpheus-ttsd` with `required-features = ["speak"]`.
- [x] `daemon/mod.rs`: the shared layer compiles with `onnx` **or** `speak`;
      `tts` only under `feature = "speak"` (the transport is cross-platform:
      UDS on Unix, AF_UNIX via `uds_windows` on Windows).
- [x] Model loaded once in the daemon (resident) from `setup.json` (`voice`),
      rebuilt per request only if it never loaded.

## 2. Configuration / feature

- [x] Reused the existing `speak` Cargo feature (no new feature).
- [x] `Setup.voice` (`setup.json` → `"voice"`): `auto_generate`, `speaker`,
      `speed`, `language`, `model_dir`. The old `enabled` and `voice` keys are
      accepted as serde aliases (`enabled` never meant "voice on/off"; `voice`
      became `speaker` so the field and the UI name the same thing).
- [x] Auto generation (`auto_generate = true`, the **default** — compiling
      with `speak` means wanting the answers spoken): generates audio in the
      background at the end of every turn — **generation only, never plays on
      its own**; the WAV is kept until the user presses `speak`/`Ctrl+Y`.
- [x] While generating: a braille spinner (the project's `SpinnerState`), no
      text, in place of the buttons; the buttons appear when it finishes.
- [x] Bass bars animate only while a playback is loaded; when the player ends
      the bars disappear and only the speak label remains.
- [x] `auto_generate = false`: generation only on demand (speak button /
      Ctrl+Y); the controls and the TTS engine stay available.
- [x] `/voice` toggles the mode in the app and persists through `Setup::save`.
- [x] Settings → Voice (gated on the `speak`/unix pair, like the engine):
      the auto-generation switch, a searchable voice picker and the pace box.
      The picker is the SAME shared list component as `/models`: rows group
      by language · model, each row names the voice and its id/gender, and
      the search matches voice name, id, language code/label, gender and
      model. Confirming a row writes `speaker` AND `language` together, so
      the user never types a numeric id or an espeak code.
- [x] Saving a voice or the pace drains the daemon so the next synthesis
      re-reads `setup.json` (the daemon loads the voice settings only at
      startup).

## 3. Audio for the last output

- [x] Triggered on `HarnessEvent::Done` with the context's last `Closure`
      (the agent's final answer).
- [x] The on-demand path uses the last closed assistant text of the session.

## 4. Context link + audio directory

- [x] `ContextItem::Closure.audio: Option<String>` (serde default,
      `skip_serializing_if` so old deltas do not grow).
- [x] `ContextManagerState::set_closure_audio(id, path)` and
      `last_closure_audio()` (the harness reads the materialized state).
- [x] `SessionStore::audio_dir(id)` =
      `{sessions_dir}/session-{id}.audio/` (next to the JSONL).
- [x] File named `closure-{context_id}.wav`.
- [x] After generation the TUI reloads the context, writes the link and saves
      again (a delta in the JSONL), so the link survives a restart or a
      restored session.
- [x] Every finalized answer keeps its own controls, so the audio of an
      earlier answer stays replayable after new turns land. The context's
      stored links are matched to the transcript messages by their final text
      (`ContextManagerState::audible_closures` + `speech_targets`).

## 5. UX

- [x] Controls anchored to the last line of EVERY visible answer, right after
      its final word — no row of their own, no reserved height and nothing in
      the transcript moves (the placeholder on the spinner row was removed).
- [x] A stored audio: only `speak`.
- [x] The answer whose clip is loaded: the bars (click restarts the clip) +
      `pause` (`resume` while paused) + `rewind`. Nothing else, and no other
      answer shows pause/rewind.
- [x] A new turn never removes the earlier answers' controls (the `speak`
      label of the previous answer stays, with its own file).
- [x] `pause` freezes/resumes the player process (SIGSTOP/SIGCONT on Unix,
      `NtSuspendProcess`/`NtResumeProcess` on Windows).
- [x] `rewind` restarts the current clip from the beginning.
- [x] `Ctrl+Y` in the prompt plays the tail's audio — **only when the prompt
      edit history has no redo** (Ctrl+Y was already redo; redo takes
      precedence so the existing feature does not regress).
- [x] Clicks resolved from the rectangles painted in the last frame
      (`SpeechState::button_at`), checked before the transcript so a button
      never starts a text selection.
- [x] No room on the final line: elements that do not fit are dropped instead
      of wrapping or moving to the next row.

## 6. Bass spinner (`agent_spinner_bass`)

- [x] `set_bass_levels(...)` control argument/method that turns off the random
      waves and uses real per-bar levels.
- [x] Bass envelope measured in the daemon from the real samples (8 low
      frequency bands, 30 fps) and returned in the response.
- [x] Bars painted inline (8 columns, `▁`…`█`) from the real envelope, in
      place of the `speak` label while the audio is loaded; clicking the bars
      replays the audio. Nothing is painted after the player ends.

## Verification (executed)

- [x] `cargo check --bin morpheus` — default without `speak`, no warnings.
- [x] `cargo check --features speak --all-targets` — includes `morpheus-ttsd`.
- [x] `cargo check --features speak,speak-phonemize --bin morpheus-ttsd` —
      the external phonemizer variant also compiles.
- [x] `cargo test --features speak --lib daemon::tts` (2) and
      `cargo test --features speak --lib speak::bass` (3) — protocol and
      envelope.
- [x] `cargo test --bin morpheus
      component::agent_spinner_bass::tests::bass_levels` (1).
- [x] `cargo test --lib harness::context::` (109 pass) — includes the new
      audio link roundtrip/serde.
- [x] `cargo test --lib session::store::tests::audio_dir` (1).
- [x] `cargo test --lib setup::` (22) — caught and fixed `merged_over`
      forgetting `voice` (a local change would be lost on a concurrent save).
- [x] `cargo test --bin morpheus --features speak speech` — visibility, anchor
      after the final word, one cluster per answer, no room left, persisted
      audio without an envelope (falls back to the `speak` label instead of the
      bars), paused and finished playback states, hit-test cleanup, and the
      answer-to-closure mapping that keeps earlier audios after a new turn.
- [x] `cargo test --bin morpheus --features speak
      routes::session::tests::answer_anchor_ends_on_the_final_answer_row`
      — each anchor ends exactly on the last text row of its answer.
- [x] `cargo test --lib harness::context::test::audible_closures` — every
      closure with a stored file is listed oldest first.
- [x] `cargo test --bin morpheus slash_menu` and
      `cargo test --bin morpheus prompt_undo` — `/voice` and the existing
      Ctrl+Y/redo do not regress.
- [x] `rustfmt --check` on the touched files (the repo's `cargo fmt` already
      had older diffs outside this scope; those were not applied).
- [x] Layout review: `session_viewport_area` and `App::render` no longer
      reserve `spinner_h` for voice; the transcript area is the same with or
      without controls, and the voice spinner (generation/playback) only
      exists inline.
- [ ] Real audio on a desktop (Kokoro model downloaded at
      `~/.cache/morpheus/models/kokoro-multi-lang-v1_1`): the players
      (`ffplay`/`aplay`/`paplay`) exist, but the default bundle is missing in
      this environment (only a `kokoro-multi-lang-v1_0` from another tool), so
      the synthesis + playback happy path was not heard. What is verified is
      the contract (files, envelope, protocol, UI states and the compilation
      of every variant).

## Decisions and limits

- Playback runs in the TUI process (a child of the system player) and the
  daemon only synthesizes/writes the WAV. That enables `pause`/`rewind` by
  signal without keeping an IPC connection open during speech.
- The controls derive from the transcript layout on every frame
  (`SessionView::answer_anchors`, one entry per visible answer) and are
  painted after the last visible glyph of each answer: they never create a row
  and never take part in any height calculation. Only answers that have a
  stored audio — and the tail, which can be generated on demand — get a
  cluster, so earlier audios stay replayable without cluttering the rest.
- Configuration lives in `setup.json` (`voice`), in Settings → Voice (which
  owns every field, including the auto-generation switch) and in `/voice`
  (a shortcut for that switch). The picker's catalog comes from the bundle
  directory name and covers the published v1.0/v1.1 voice tables; a renamed
  bundle still works through the raw `speaker` id, the list just cannot name
  the voices. A voice-field save drains the resident daemon because it
  snapshots the settings at startup; the next synthesis activates a fresh
  one.
- The TTS daemon and the voice controls are built wherever the `speak`
  feature is on: the daemon transport is AF_UNIX on Unix and on Windows
  (Windows 10 1803+), and `pause` uses `SIGSTOP`/`SIGCONT` on Unix and the
  process-wide thread suspension of `NtSuspendProcess`/`NtResumeProcess` on
  Windows.
- The bass envelope is a real low-band energy measurement (band-pass obtained
  from the difference of neighbouring low-pass filters, not an FFT), enough
  for 8 bars and with no new dependency. The first version used the plain
  low-pass and the tests showed the bars only grew to the right (every wide
  band contains the narrow bands' energy); the difference between cutoffs is
  what separates the bands.
