//! Voice controls anchored to the final answer of every message.
//!
//! The daemon synthesizes the WAV (`morpheus::daemon::tts`); this module
//! tracks which answers can be spoken, the generation in flight and the
//! player handle that pause/rewind act on. Each control cluster is painted on
//! the row that holds the final word of its answer, right after it — never on
//! a row of its own, so nothing in the transcript moves when the controls
//! appear, change width or disappear. Older answers keep their controls, so
//! their audio stays replayable after a new turn lands.

use std::path::PathBuf;
#[cfg(feature = "speak")]
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
#[cfg(feature = "speak")]
use ratatui::style::Style;

#[cfg(feature = "speak")]
use crate::component::agent_spinner_bass::AgentSpinnerBass;
#[cfg(feature = "speak")]
use crate::component::spinner::SpinnerState;
use crate::theme::Theme;
#[cfg(feature = "speak")]
use crate::theme::rgba_color;

/// Equalizer bars and animation rate requested from the daemon.
const BARS: u32 = 8;
const FPS: u32 = 30;
/// Blank columns between two painted elements of the control cluster.
const GAP: u16 = 2;

/// One of the buttons in an answer's controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechButton {
    Speak,
    Pause,
    Rewind,
}

/// A finalized answer that can carry audio, tied to its transcript message.
///
/// `slot` is the message index in the session, so the controls follow the
/// answer while the transcript scrolls. `closure_id` is the context closure
/// whose stored `audio` link belongs here; it is `None` for an answer the
/// context no longer carries (for example a loop that ended on a tool call).
#[derive(Debug, Clone)]
pub struct VoiceTarget {
    pub slot: usize,
    pub closure_id: Option<u64>,
    pub text: String,
    pub audio: Option<PathBuf>,
}

/// What a finished (or failed) generation reports back to the App so it can
/// link the file into the session context and toast failures.
#[derive(Debug)]
pub struct SpeechFinished {
    pub session_id: String,
    pub closure_id: Option<u64>,
    pub wav: Option<PathBuf>,
    pub error: Option<String>,
}

/// A generated utterance plus its measured bass envelope.
#[cfg(feature = "speak")]
#[derive(Debug, Clone)]
struct Clip {
    wav: PathBuf,
    bass: Vec<f32>,
    bars: usize,
    fps: u32,
}

#[cfg(feature = "speak")]
struct Active {
    clip: Clip,
    playback: Option<morpheus::speak::Playback>,
    /// The answer this clip belongs to; its controls show pause/rewind.
    slot: usize,
    /// Elapsed at the moment of the pause; `None` while running.
    paused_at: Option<Duration>,
    started: Instant,
}

#[cfg(feature = "speak")]
impl Active {
    fn elapsed(&self) -> Duration {
        self.paused_at.unwrap_or_else(|| self.started.elapsed())
    }

    fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    fn levels(&self) -> Vec<f32> {
        if self.clip.bars == 0 || self.clip.fps == 0 || self.clip.bass.len() < self.clip.bars {
            return Vec::new();
        }
        let frame = (self.elapsed().as_secs_f32() * self.clip.fps as f32) as usize;
        let frames = self.clip.bass.len() / self.clip.bars;
        let frame = frame.min(frames.saturating_sub(1));
        let start = frame * self.clip.bars;
        self.clip.bass[start..start + self.clip.bars].to_vec()
    }
}

#[cfg(feature = "speak")]
struct Generation {
    rx: std::sync::mpsc::Receiver<
        Result<morpheus::daemon::tts::protocol::SynthesizeResult, String>,
    >,
    session_id: String,
    closure_id: Option<u64>,
    slot: usize,
    /// Start playing as soon as the WAV exists. `false` for the background
    /// generation at the end of a turn: the file is kept for the Speak button
    /// / Ctrl+Y, never played unasked.
    play: bool,
}

/// Built without the voice engine: the controls are never drawn and every
/// action is a no-op, so the rest of the App needs no `cfg` noise.
const AVAILABLE: bool = cfg!(feature = "speak");

/// Voice state attached to the App.
pub struct SpeechState {
    auto_generate: bool,
    session_id: Option<String>,
    targets: Vec<VoiceTarget>,
    button_rects: Vec<(SpeechButton, usize, Rect)>,
    #[cfg(feature = "speak")]
    spinner: SpinnerState,
    /// The bass equalizer that takes the speak button's place while a clip is
    /// playing, driven by the measured low end of the real speech.
    #[cfg(feature = "speak")]
    bass: Option<AgentSpinnerBass>,
    #[cfg(feature = "speak")]
    request: Option<Generation>,
    #[cfg(feature = "speak")]
    active: Option<Active>,
    /// A finished background generation waiting for play, with the answer it
    /// belongs to. Kept so the first press already paints the measured bass.
    #[cfg(feature = "speak")]
    prepared: Option<(usize, Clip)>,
}

impl Default for SpeechState {
    fn default() -> Self {
        Self {
            auto_generate: false,
            session_id: None,
            targets: Vec::new(),
            button_rects: Vec::new(),
            #[cfg(feature = "speak")]
            spinner: SpinnerState::new(),
            #[cfg(feature = "speak")]
            bass: None,
            #[cfg(feature = "speak")]
            request: None,
            #[cfg(feature = "speak")]
            active: None,
            #[cfg(feature = "speak")]
            prepared: None,
        }
    }
}

impl SpeechState {
    #[must_use]
    pub fn new(auto_generate: bool) -> Self {
        Self {
            auto_generate,
            ..Self::default()
        }
    }

    #[must_use]
    pub const fn auto_generate(&self) -> bool {
        self.auto_generate
    }

    pub const fn set_auto_generate(&mut self, auto_generate: bool) {
        self.auto_generate = auto_generate;
    }

    /// Point the controls at `session_id`'s answers. Answers without text are
    /// dropped: there is nothing to speak.
    pub fn set_targets(&mut self, session_id: &str, targets: Vec<VoiceTarget>) {
        if self.session_id.as_deref() != Some(session_id) {
            self.stop();
            self.session_id = Some(session_id.to_owned());
        }
        self.targets = targets
            .into_iter()
            .filter(|target| !target.text.trim().is_empty())
            .collect();
    }

    /// Forget every answer (session switch, or no finalized answer at all).
    pub fn clear_targets(&mut self) {
        self.stop();
        self.targets.clear();
    }

    /// Whether the controls belong on screen. `auto_generate` is only the
    /// auto-generation switch: with it off they still appear so the audio can
    /// be asked for on demand.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        AVAILABLE && !self.targets.is_empty()
    }

    #[must_use]
    pub fn is_generating(&self) -> bool {
        #[cfg(feature = "speak")]
        {
            self.request.is_some()
        }
        #[cfg(not(feature = "speak"))]
        {
            false
        }
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        #[cfg(feature = "speak")]
        {
            self.active.as_ref().is_some_and(Active::is_paused)
        }
        #[cfg(not(feature = "speak"))]
        {
            false
        }
    }

    /// Whether a clip is loaded in the player: playing or paused. Only then
    /// do pause/rewind belong on screen, and only then does the speak label
    /// give way to the bars.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        #[cfg(feature = "speak")]
        {
            self.active
                .as_ref()
                .is_some_and(|active| active.playback.is_some())
        }
        #[cfg(not(feature = "speak"))]
        {
            false
        }
    }

    /// Continuous frames are needed while synthesizing or playing; a paused
    /// clip needs none (its bars are frozen).
    #[must_use]
    pub fn is_animating(&self) -> bool {
        #[cfg(feature = "speak")]
        {
            self.request.is_some()
                || self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.playback.is_some() && !active.is_paused())
        }
        #[cfg(not(feature = "speak"))]
        {
            false
        }
    }

    /// Generate the tail's audio in the background without playing it. This
    /// is what the end of a turn does: the WAV is left ready (and linked into
    /// the context), and only the user starts playback.
    pub fn generate_tail(&mut self, store: &morpheus::session::store::SessionStore) {
        let Some(target) = self.targets.last().cloned() else {
            return;
        };
        if self.is_generating() {
            return;
        }
        if target.audio.as_ref().is_some_and(|path| path.exists()) {
            return;
        }
        #[cfg(feature = "speak")]
        self.spawn_generation(&target, store, false);
        #[cfg(not(feature = "speak"))]
        let _ = store;
    }

    /// Ask for the tail's audio (Speak button / Ctrl+Y).
    pub fn speak_tail(&mut self, store: &morpheus::session::store::SessionStore) {
        let Some(slot) = self.targets.last().map(|target| target.slot) else {
            return;
        };
        self.speak(slot, store);
    }

    /// Play the answer at `slot`: the persisted file when there is one, or a
    /// synthesis that starts playing as soon as it is ready.
    pub fn speak(&mut self, slot: usize, store: &morpheus::session::store::SessionStore) {
        let Some(target) = self
            .targets
            .iter()
            .find(|target| target.slot == slot)
            .cloned()
        else {
            return;
        };
        if target.text.trim().is_empty() {
            return;
        }
        #[cfg(feature = "speak")]
        if let Some(request) = self.request.as_mut() {
            if request.slot == slot {
                request.play = true;
            }
            return;
        }
        #[cfg(feature = "speak")]
        if let Some(path) = target.audio.as_ref().filter(|path| path.exists()) {
            self.play(path.clone(), None, slot);
            return;
        }
        #[cfg(feature = "speak")]
        self.spawn_generation(&target, store, true);
        #[cfg(not(feature = "speak"))]
        let _ = store;
    }

    /// Pause/resume the active playback.
    pub fn toggle_pause(&mut self) {
        #[cfg(feature = "speak")]
        if let Some(active) = self.active.as_mut() {
            if active.is_paused() {
                if let Some(playback) = active.playback.as_mut()
                    && playback.resume()
                {
                    let paused = active.paused_at.take().unwrap_or_default();
                    active.started = Instant::now()
                        .checked_sub(paused)
                        .unwrap_or_else(Instant::now);
                }
            } else if let Some(playback) = active.playback.as_mut()
                && playback.pause()
            {
                active.paused_at = Some(active.started.elapsed());
            }
        }
    }

    /// Restart the active clip from the beginning.
    pub fn rewind(&mut self) {
        #[cfg(feature = "speak")]
        if let Some(active) = &self.active {
            let wav = active.clip.wav.clone();
            let clip = active.clip.clone();
            let slot = active.slot;
            self.play(wav, Some(clip), slot);
        }
    }

    /// Advance the generation spinner and the playback state. Returns a
    /// finished generation exactly once so the App can persist the link /
    /// surface the error.
    pub fn tick(&mut self) -> Option<SpeechFinished> {
        #[cfg(feature = "speak")]
        {
            if self.request.is_some() {
                self.spinner.advance();
            }

            let mut finished = None;
            let received = self
                .request
                .as_ref()
                .and_then(|request| match request.rx.try_recv() {
                    Ok(result) => Some(result),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(
                        "voice daemon connection closed before answering".to_string(),
                    )),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                });
            if let Some(result) = received {
                let generation = self.request.take().expect("checked above");
                match result {
                    Ok(result) => {
                        let wav = PathBuf::from(&result.wav);
                        if let Some(target) = self
                            .targets
                            .iter_mut()
                            .find(|target| target.slot == generation.slot)
                        {
                            target.audio = Some(wav.clone());
                        }
                        let clip = Clip {
                            wav: wav.clone(),
                            bass: result.bass,
                            bars: result.bars as usize,
                            fps: result.fps,
                        };
                        let error = if generation.play {
                            self.start_clip(clip, generation.slot)
                        } else {
                            self.prepared = Some((generation.slot, clip));
                            None
                        };
                        finished = Some(SpeechFinished {
                            session_id: generation.session_id,
                            closure_id: generation.closure_id,
                            wav: Some(wav),
                            error,
                        });
                    }
                    Err(error) => {
                        finished = Some(SpeechFinished {
                            session_id: generation.session_id,
                            closure_id: generation.closure_id,
                            wav: None,
                            error: Some(error),
                        });
                    }
                }
            }

            if let Some(active) = self.active.as_mut()
                && active
                    .playback
                    .as_mut()
                    .is_some_and(|playback| playback.is_finished())
            {
                active.playback = None;
                active.paused_at = None;
            }

            return finished;
        }
        #[cfg(not(feature = "speak"))]
        {
            None
        }
    }

    /// Paint the controls after the final word of every visible answer in
    /// `anchors` (message slot plus its screen rows) and remember their
    /// rectangles for the mouse.
    pub fn render_inline(&mut self, buf: &mut Buffer, anchors: &[(usize, Rect)], theme: &Theme) {
        self.button_rects.clear();
        #[cfg(feature = "speak")]
        self.draw_inline(buf, anchors, theme);
        #[cfg(not(feature = "speak"))]
        let _ = (buf, anchors, theme);
    }

    #[cfg(feature = "speak")]
    fn draw_inline(&mut self, buf: &mut Buffer, anchors: &[(usize, Rect)], theme: &Theme) {
        for &(slot, area) in anchors {
            let Some(index) = self.targets.iter().position(|target| target.slot == slot) else {
                continue;
            };
            self.draw_cluster(buf, index, area, theme);
        }
    }

    #[cfg(feature = "speak")]
    fn draw_cluster(&mut self, buf: &mut Buffer, index: usize, area: Rect, theme: &Theme) {
        let Some(target) = self.targets.get(index).cloned() else {
            return;
        };
        let Some((glyph_x, y)) = last_glyph(buf, area) else {
            return;
        };
        let end = area.right().saturating_sub(1);
        let mut x = glyph_x.saturating_add(2);
        if x >= end {
            return;
        }

        if self
            .request
            .as_ref()
            .is_some_and(|generation| generation.slot == target.slot)
        {
            let glyph = self.spinner.current_char().to_string();
            buf.set_string(x, y, glyph, Style::default().fg(rgba_color(theme.primary)));
            return;
        }

        if let Some(levels) = self
            .playback_levels(target.slot)
            .filter(|levels| !levels.is_empty())
        {
            // The real bass of the utterance replaces the speak button while
            // the clip plays: the bass equalizer runs in its non-random mode,
            // every bar as tall as the measured level. Clicking it replays.
            let bars = {
                let spinner = self.bass_spinner(theme);
                spinner.set_bass_levels(Some(levels));
                spinner.render(buf, x, y);
                spinner.width()
            };
            #[allow(clippy::cast_possible_truncation)]
            self.button_rects.push((
                SpeechButton::Speak,
                target.slot,
                Rect::new(x, y, bars as u16, 1),
            ));
            x = x.saturating_add(bars as u16).saturating_add(GAP);
        } else {
            let Some(next) = self.draw_button(
                buf,
                x,
                y,
                target.slot,
                SpeechButton::Speak,
                "speak",
                theme.primary,
                end,
            ) else {
                return;
            };
            x = next;
        }

        if !self.is_playing_at(target.slot) {
            return;
        }
        let pause = if self.is_paused() { "resume" } else { "pause" };
        let Some(next) = self.draw_button(
            buf,
            x,
            y,
            target.slot,
            SpeechButton::Pause,
            pause,
            theme.text_muted,
            end,
        ) else {
            return;
        };
        self.draw_button(
            buf,
            next,
            y,
            target.slot,
            SpeechButton::Rewind,
            "rewind",
            theme.text_muted,
            end,
        );
    }

    #[cfg(feature = "speak")]
    fn draw_button(
        &mut self,
        buf: &mut Buffer,
        x: u16,
        y: u16,
        slot: usize,
        button: SpeechButton,
        label: &str,
        color: morpheus_tui::core::lib::rgba::RGBA,
        end: u16,
    ) -> Option<u16> {
        let width = u16::try_from(label.chars().count()).ok()?;
        if x.saturating_add(width) > end {
            return None;
        }
        buf.set_string(x, y, label, Style::default().fg(rgba_color(color)));
        self.button_rects
            .push((button, slot, Rect::new(x, y, width, 1)));
        Some(x.saturating_add(width).saturating_add(GAP))
    }

    #[cfg(feature = "speak")]
    fn bass_spinner(&mut self, theme: &Theme) -> &mut AgentSpinnerBass {
        let spinner = self.bass.get_or_insert_with(|| {
            let mut spinner = AgentSpinnerBass::new("", theme);
            spinner.set_mode_color(theme.primary);
            spinner
        });
        spinner.set_mode_color(theme.primary);
        spinner
    }

    #[cfg(feature = "speak")]
    fn playback_levels(&self, slot: usize) -> Option<Vec<f32>> {
        let active = self.active.as_ref()?;
        if active.slot != slot {
            return None;
        }
        active.playback.as_ref()?;
        Some(active.levels())
    }

    #[cfg(feature = "speak")]
    fn is_playing_at(&self, slot: usize) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.slot == slot && active.playback.is_some())
    }

    /// The button under a mouse position and the answer it belongs to, from
    /// the last rendered frame.
    #[must_use]
    pub fn button_at(&self, x: u16, y: u16) -> Option<(SpeechButton, usize)> {
        self.button_rects.iter().find_map(|(button, slot, rect)| {
            (x >= rect.x && x < rect.x + rect.width && y == rect.y).then_some((*button, *slot))
        })
    }

    /// Run the action for a button of the answer at `slot`.
    pub fn activate(
        &mut self,
        button: SpeechButton,
        slot: usize,
        store: &morpheus::session::store::SessionStore,
    ) {
        match button {
            SpeechButton::Speak => self.speak(slot, store),
            SpeechButton::Pause => self.toggle_pause(),
            SpeechButton::Rewind => self.rewind(),
        }
    }

    /// Stop and forget the player (used on teardown and session changes).
    pub fn stop(&mut self) {
        #[cfg(feature = "speak")]
        {
            self.active = None;
            self.request = None;
            self.prepared = None;
            self.bass = None;
        }
    }

    #[cfg(feature = "speak")]
    fn spawn_generation(
        &mut self,
        target: &VoiceTarget,
        store: &morpheus::session::store::SessionStore,
        play: bool,
    ) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let output = store.audio_dir(&session_id).join(format!(
            "closure-{}.wav",
            target
                .closure_id
                .map_or_else(|| "tail".to_string(), |id| id.to_string())
        ));
        if let Some(parent) = output.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let text = target.text.clone();
        let expected = output.clone();
        std::thread::spawn(move || {
            let client = morpheus::daemon::tts::TtsClient::new();
            let result = client.synthesize(&text, &expected, None, None, BARS, FPS);
            let _ = tx.send(result);
        });
        self.request = Some(Generation {
            rx,
            session_id,
            closure_id: target.closure_id,
            slot: target.slot,
            play,
        });
    }

    #[cfg(feature = "speak")]
    fn play(&mut self, wav: PathBuf, envelope: Option<Clip>, slot: usize) {
        let clip = envelope
            .or_else(|| self.clip_for(&wav, slot))
            .unwrap_or_else(|| Clip {
                wav,
                bass: Vec::new(),
                bars: BARS as usize,
                fps: FPS,
            });
        self.prepared = None;
        self.start_clip(clip, slot);
    }

    #[cfg(feature = "speak")]
    fn clip_for(&self, wav: &std::path::Path, slot: usize) -> Option<Clip> {
        if let Some(active) = &self.active
            && active.slot == slot
            && active.clip.wav == wav
        {
            return Some(active.clip.clone());
        }
        let (prepared_slot, prepared) = self.prepared.as_ref()?;
        (*prepared_slot == slot && prepared.wav == wav).then(|| prepared.clone())
    }

    /// Start (or restart) playback for `clip` and return the player error, if
    /// any. The clip is kept without a player so rewind can retry.
    #[cfg(feature = "speak")]
    fn start_clip(&mut self, clip: Clip, slot: usize) -> Option<String> {
        let playback = morpheus::speak::Playback::start(&clip.wav);
        let error = playback.as_ref().err().map(ToString::to_string);
        self.active = Some(Active {
            clip,
            playback: playback.ok(),
            slot,
            paused_at: None,
            started: Instant::now(),
        });
        error
    }
}

/// Coordinates of the last painted glyph inside `area`, scanning the bottom
/// row first. The rightmost column is skipped: it is the session scrollbar.
fn last_glyph(buf: &Buffer, area: Rect) -> Option<(u16, u16)> {
    let left = area.x;
    let right = area.right().saturating_sub(1).min(buf.area.right());
    let bottom = area.bottom().min(buf.area.bottom());
    for y in (area.y..bottom).rev() {
        for x in (left..right).rev() {
            let Some(cell) = buf.cell((x, y)) else {
                continue;
            };
            if !cell.symbol().trim().is_empty() {
                return Some((x, y));
            }
        }
    }
    None
}

#[cfg(all(test, feature = "speak"))]
mod tests {
    use super::*;

    fn theme() -> Theme {
        crate::theme::ThemeRegistry::new()
            .get("opencode")
            .cloned()
            .unwrap()
    }

    fn target(slot: usize, text: &str, audio: Option<&str>) -> VoiceTarget {
        VoiceTarget {
            slot,
            closure_id: Some(slot as u64 + 1),
            text: text.to_string(),
            audio: audio.map(PathBuf::from),
        }
    }

    fn answer_area(width: u16) -> (Buffer, Rect) {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(2, 0, "the answer", Style::default());
        (buf, area)
    }

    fn loaded_clip(speech: &mut SpeechState, slot: usize) {
        speech.active = Some(Active {
            clip: Clip {
                wav: "/tmp/a.wav".into(),
                bass: vec![0.0; BARS as usize * 3],
                bars: BARS as usize,
                fps: FPS,
            },
            playback: Some(morpheus::speak::Playback::idle_for_test()),
            slot,
            paused_at: None,
            started: Instant::now(),
        });
    }

    #[test]
    fn controls_need_a_finalized_answer() {
        let mut speech = SpeechState::new(false);
        assert!(!speech.is_visible());
        speech.set_targets("s1", vec![target(0, "   ", None)]);
        assert!(!speech.is_visible());
        speech.set_targets("s1", vec![target(0, "the answer", None)]);
        assert!(speech.is_visible());
        assert!(!speech.auto_generate());
        speech.set_targets("s2", Vec::new());
        assert!(!speech.is_visible());
    }

    #[test]
    fn buttons_follow_the_final_word() {
        let mut speech = SpeechState::new(true);
        speech.set_targets("s1", vec![target(0, "the answer", Some("/tmp/a.wav"))]);
        let (mut buf, area) = answer_area(60);

        speech.render_inline(&mut buf, &[(0, area)], &theme());

        // "the answer" ends at column 11; the cluster starts two cells later.
        assert_eq!(speech.button_at(13, 0), Some((SpeechButton::Speak, 0)));
        assert_eq!(speech.button_at(12, 0), None);
        assert_eq!(speech.button_at(13, 1), None);
    }

    #[test]
    fn every_answer_keeps_its_own_controls() {
        let mut speech = SpeechState::new(true);
        speech.set_targets(
            "s1",
            vec![
                target(0, "the answer", Some("/tmp/first.wav")),
                target(1, "the answer", Some("/tmp/second.wav")),
            ],
        );
        let area = Rect::new(0, 0, 60, 4);
        let mut buf = Buffer::empty(area);
        buf.set_string(2, 0, "the answer", Style::default());
        buf.set_string(2, 3, "the answer", Style::default());

        speech.render_inline(
            &mut buf,
            &[(0, Rect::new(0, 0, 60, 1)), (1, Rect::new(0, 3, 60, 1))],
            &theme(),
        );

        assert_eq!(speech.button_at(13, 0), Some((SpeechButton::Speak, 0)));
        assert_eq!(speech.button_at(13, 3), Some((SpeechButton::Speak, 1)));
        assert_eq!(
            speech.targets[0].audio.as_deref(),
            Some(std::path::Path::new("/tmp/first.wav"))
        );
        assert_eq!(
            speech.targets[1].audio.as_deref(),
            Some(std::path::Path::new("/tmp/second.wav"))
        );
    }

    #[test]
    fn pause_and_rewind_wait_for_a_running_clip() {
        let mut speech = SpeechState::new(true);
        speech.set_targets("s1", vec![target(0, "the answer", Some("/tmp/a.wav"))]);

        // A stored audio the user has not started: only speak is offered.
        let (mut buf, area) = answer_area(60);
        speech.render_inline(&mut buf, &[(0, area)], &theme());
        assert!(!speech.is_playing());
        assert!(
            speech
                .button_rects
                .iter()
                .all(|(button, _, _)| *button == SpeechButton::Speak)
        );

        loaded_clip(&mut speech, 0);
        let (mut buf, area) = answer_area(60);
        speech.render_inline(&mut buf, &[(0, area)], &theme());
        assert!(speech.is_playing());
        let bars_end = 13 + BARS as u16;
        assert_eq!(
            speech.button_at(bars_end + GAP, 0),
            Some((SpeechButton::Pause, 0))
        );
        assert_eq!(
            speech.button_at(bars_end + GAP + 5 + GAP, 0),
            Some((SpeechButton::Rewind, 0))
        );

        // The player is gone even though the clip is loaded: back to speak.
        speech.active.as_mut().expect("clip").playback = None;
        let (mut buf, area) = answer_area(60);
        speech.render_inline(&mut buf, &[(0, area)], &theme());
        assert!(!speech.is_playing());
        assert!(
            speech
                .button_rects
                .iter()
                .all(|(button, _, _)| *button == SpeechButton::Speak)
        );
    }

    #[test]
    fn the_bass_spinner_replaces_speak_while_playing_and_yields_to_it_after() {
        let mut speech = SpeechState::new(true);
        speech.set_targets("s1", vec![target(0, "the answer", Some("/tmp/a.wav"))]);
        speech.active = Some(Active {
            clip: Clip {
                wav: "/tmp/a.wav".into(),
                bass: vec![1.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                bars: BARS as usize,
                fps: FPS,
            },
            playback: Some(morpheus::speak::Playback::idle_for_test()),
            slot: 0,
            paused_at: None,
            started: Instant::now(),
        });

        // While the clip plays the bars take the speak button's place, each
        // exactly as tall as the real level measured for that band.
        let (mut buf, area) = answer_area(60);
        speech.render_inline(&mut buf, &[(0, area)], &theme());
        assert_eq!(buf.cell((13, 0)).unwrap().symbol(), "█");
        assert_eq!(buf.cell((14, 0)).unwrap().symbol(), "▅");
        assert_eq!(buf.cell((15, 0)).unwrap().symbol(), "▁");
        assert_eq!(speech.button_at(13, 0), Some((SpeechButton::Speak, 0)));

        // The player ends: the bars go away and the speak label returns.
        speech.active.as_mut().expect("clip").playback = None;
        let (mut buf, area) = answer_area(60);
        speech.render_inline(&mut buf, &[(0, area)], &theme());
        assert_eq!(buf.cell((13, 0)).unwrap().symbol(), "s");
        assert_eq!(speech.button_at(13, 0), Some((SpeechButton::Speak, 0)));
    }

    #[test]
    fn controls_are_dropped_when_the_last_line_has_no_room() {
        let mut speech = SpeechState::new(true);
        speech.set_targets("s1", vec![target(0, "the answer", None)]);
        let area = Rect::new(0, 0, 14, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(2, 0, "the answer", Style::default());

        speech.render_inline(&mut buf, &[(0, area)], &theme());

        assert!(speech.button_at(13, 0).is_none());
        assert!(speech.button_rects.is_empty());
    }

    #[test]
    fn an_audio_without_an_envelope_has_no_bars() {
        let mut speech = SpeechState::new(true);
        speech.set_targets("s1", vec![target(0, "the answer", Some("/tmp/a.wav"))]);
        speech.active = Some(Active {
            clip: Clip {
                wav: "/tmp/a.wav".into(),
                bass: Vec::new(),
                bars: BARS as usize,
                fps: FPS,
            },
            playback: None,
            slot: 0,
            paused_at: None,
            started: Instant::now(),
        });

        assert!(speech.active.as_ref().unwrap().levels().is_empty());
        assert_eq!(speech.playback_levels(0), None);
    }

    #[test]
    fn hiding_the_controls_clears_the_hit_test_surface() {
        let mut speech = SpeechState::new(true);
        speech.set_targets("s1", vec![target(0, "the answer", None)]);
        let (mut buf, area) = answer_area(40);
        speech.render_inline(&mut buf, &[(0, area)], &theme());
        assert!(!speech.button_rects.is_empty());

        speech.render_inline(&mut buf, &[], &theme());
        assert!(speech.button_rects.is_empty());
    }
}
