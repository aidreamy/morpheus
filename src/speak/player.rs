//! Blocking playback through the audio player the machine already has.
//!
//! sherpa-onnx generates samples but intentionally opens no audio device; the
//! crate has no player. Linking `cpal`/`rodio` would put an ALSA development
//! header (`libasound2-dev`) in the build requirements of every platform, so
//! `speak` writes the utterance to a temporary WAV and hands it to the
//! platform's own command-line player instead. The dependency-free result
//! covers the desktop cases the TUI needs; a caller that wants gapless,
//! chunk-by-chunk output can drive [`Speak::synthesize_streaming`] with its
//! own sink.
//!
//! [`Speak::synthesize_streaming`]: super::Speak::synthesize_streaming

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::config::SpeakError;
use super::engine::Speech;

/// Serializes the temporary file names of the utterances played at once.
static COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn play(speech: &Speech) -> Result<(), SpeakError> {
    let path = temp_path();
    if let Err(error) = speech.save_wav(&path) {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    let result = play_file(&path);
    let _ = fs::remove_file(&path);
    result
}

/// A live playback process the caller can pause, resume and restart.
///
/// The player binaries are external commands (see the module docs), so the
/// only portable pause primitive is a job-control signal: `SIGSTOP` freezes
/// the process and with it the audio device feed, `SIGCONT` resumes it. On
/// platforms without that signal (Windows) the controls report failure and
/// the caller falls back to stop/start.
///
/// The WAV is opened by the player at spawn, so the caller is free to remove
/// or replace the file afterwards.
pub struct Playback {
    child: Child,
    paused: bool,
}

impl Playback {
    /// Start playing `path`, trying each available system player until one
    /// stays alive. The failed candidates are reaped before the next one is
    /// tried.
    pub fn start(path: &Path) -> Result<Self, SpeakError> {
        let mut tried = Vec::new();
        for mut command in commands(path) {
            let name = command.get_program().to_string_lossy().into_owned();
            match command.spawn() {
                Ok(mut child) => {
                    // A player that immediately exits (no sound server, closed
                    // device) is not "playing": try the next candidate.
                    std::thread::sleep(Duration::from_millis(120));
                    match child.try_wait() {
                        Ok(Some(status)) if !status.success() => {
                            tried.push(format!("{name}: {status}"));
                        }
                        Ok(Some(_)) | Ok(None) => {
                            return Ok(Self {
                                child,
                                paused: false,
                            });
                        }
                        Err(error) => tried.push(format!("{name}: {error}")),
                    }
                }
                Err(error) => tried.push(format!("{name}: {error}")),
            }
        }
        Err(SpeakError::NoPlayer(tried.join(", ")))
    }

    /// Whether this playback is currently paused.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        self.paused
    }

    /// Pause the audio. Returns `false` when the process is already gone or
    /// the platform has no pause primitive.
    pub fn pause(&mut self) -> bool {
        if self.paused || self.is_finished() {
            return false;
        }
        if signal(&self.child, STOP_SIGNAL) {
            self.paused = true;
            true
        } else {
            false
        }
    }

    /// Resume a paused playback.
    pub fn resume(&mut self) -> bool {
        if !self.paused {
            return false;
        }
        if signal(&self.child, CONT_SIGNAL) {
            self.paused = false;
            true
        } else {
            false
        }
    }

    /// Stop playback now (best effort; the reap happens in [`Self::is_finished`]
    /// or on drop).
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.paused = false;
    }

    /// Whether the player process has exited. Reaps the child when it has.
    pub fn is_finished(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(Some(_)) | Err(_) => true,
            Ok(None) => false,
        }
    }
}

impl Playback {
    /// A player that stays alive until dropped, for tests that only inspect
    /// the playback state and never touch a sound device.
    #[doc(hidden)]
    pub fn idle_for_test() -> Self {
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        Self {
            child,
            paused: false,
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // A playback dropped while still running must not outlive the screen
        // that owns it. Killing a finished child is harmless.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
const STOP_SIGNAL: i32 = libc::SIGSTOP;
#[cfg(unix)]
const CONT_SIGNAL: i32 = libc::SIGCONT;
#[cfg(not(unix))]
const STOP_SIGNAL: i32 = 0;
#[cfg(not(unix))]
const CONT_SIGNAL: i32 = 0;

#[cfg(unix)]
fn signal(child: &Child, sig: i32) -> bool {
    // `kill(2)` on the child pid: the process may already have exited.
    unsafe { libc::kill(child.id() as libc::pid_t, sig) == 0 }
}

#[cfg(not(unix))]
fn signal(_child: &Child, _signal: i32) -> bool {
    false
}

fn temp_path() -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "morpheus-speak-{}-{unique}.wav",
        std::process::id()
    ))
}

/// Try each player in turn. A player that is absent is skipped; one that
/// exits unsuccessfully (no sound server, closed device) is recorded and the
/// next candidate gets a chance.
fn play_file(path: &Path) -> Result<(), SpeakError> {
    let mut tried = Vec::new();
    for mut command in commands(path) {
        let name = format!("{}", command.get_program().to_string_lossy());
        match command.status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => tried.push(format!("{name}: {status}")),
            Err(error) => tried.push(format!("{name}: {error}")),
        }
    }
    Err(SpeakError::NoPlayer(tried.join(", ")))
}

#[cfg(target_os = "macos")]
fn commands(path: &Path) -> Vec<Command> {
    let mut afplay = Command::new("afplay");
    afplay.arg(path);
    let mut ffplay = ffplay(path);
    vec![afplay, ffplay]
}

#[cfg(windows)]
fn commands(path: &Path) -> Vec<Command> {
    // `SoundPlayer.PlaySync` blocks like the Unix players do. Single quotes
    // are doubled to stay literal inside the PowerShell string.
    let literal = path.to_string_lossy().replace('\'', "''");
    let mut powershell = Command::new("powershell");
    powershell.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &format!("(New-Object Media.SoundPlayer '{literal}').PlaySync()"),
    ]);
    vec![powershell]
}

#[cfg(not(any(target_os = "macos", windows)))]
fn commands(path: &Path) -> Vec<Command> {
    let mut paplay = Command::new("paplay");
    paplay.arg(path);

    let mut pw_play = Command::new("pw-play");
    pw_play.arg(path);

    let mut aplay = Command::new("aplay");
    aplay.args(["-q"]).arg(path);

    let mut play = Command::new("play");
    play.args(["-q"]).arg(path);

    vec![paplay, pw_play, aplay, play, ffplay(path)]
}

#[cfg(not(windows))]
fn ffplay(path: &Path) -> Command {
    let mut command = Command::new("ffplay");
    command
        .args(["-nodisp", "-autoexit", "-loglevel", "quiet"])
        .arg(path);
    command
}
