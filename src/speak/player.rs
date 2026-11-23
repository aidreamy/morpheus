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
/// The player binaries are external commands (see the module docs), so pause
/// has to freeze the process itself: `SIGSTOP` on Unix and the process-wide
/// thread suspension ntdll exposes on Windows. Either way the audio device
/// stops being fed while the player is frozen, and thawing it carries on from
/// the same sample.
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
        if freeze(&self.child, true) {
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
        if freeze(&self.child, false) {
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
        // Any long-running process stands in for the player. `sleep` is not a
        // binary on Windows, so the loopback ping that drains its count
        // takes over there.
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("ping");
            command.args(["-n", "31", "127.0.0.1"]);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = Command::new("sleep");
            command.arg("30");
            command
        };
        let child = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the idle test process");
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

/// Freeze (`suspend`) or thaw the player process.
///
/// Unix job control is the portable-enough primitive: `SIGSTOP` stops every
/// thread of the process, `SIGCONT` resumes them.
#[cfg(unix)]
fn freeze(child: &Child, suspend: bool) -> bool {
    let signal = if suspend {
        libc::SIGSTOP
    } else {
        libc::SIGCONT
    };
    // `kill(2)` on the child pid: the process may already have exited.
    unsafe { libc::kill(child.id() as libc::pid_t, signal) == 0 }
}

/// Windows has no job-control signal, so the equivalent of `SIGSTOP`/`SIGCONT`
/// is the process-wide thread suspension ntdll exposes. While the player's
/// threads are frozen it stops feeding the device; the samples already handed
/// to the driver may still be audible for a moment.
#[cfg(windows)]
fn freeze(child: &Child, suspend: bool) -> bool {
    use std::os::windows::io::AsRawHandle;

    // Ntdll exports these for the debugger/Process Explorer "Suspend" action.
    // `windows-sys` surfaces thread-level suspension only, so the process-wide
    // pair is linked directly.
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSuspendProcess(process: *mut core::ffi::c_void) -> i32;
        fn NtResumeProcess(process: *mut core::ffi::c_void) -> i32;
    }

    let process = child.as_raw_handle().cast::<core::ffi::c_void>();
    if process.is_null() {
        return false;
    }
    let status = unsafe {
        if suspend {
            NtSuspendProcess(process)
        } else {
            NtResumeProcess(process)
        }
    };
    // NTSTATUS: zero or any positive informational code is a success.
    status >= 0
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `pause`/`resume` must reach the OS (no player fixture and no sound
    /// device are needed): the process is frozen and stays alive, then thawed.
    #[test]
    fn pause_freezes_and_resume_thaws_the_player() {
        let mut playback = Playback::idle_for_test();
        assert!(!playback.is_paused());
        assert!(!playback.resume(), "resume without a pause is refused");

        assert!(playback.pause(), "the platform freezes the player");
        assert!(playback.is_paused());
        assert!(!playback.pause(), "a second pause is refused");
        assert!(
            !playback.is_finished(),
            "a frozen player is alive, not finished"
        );

        assert!(playback.resume(), "the platform thaws the player");
        assert!(!playback.is_paused());
        assert!(!playback.is_finished());
    }
}

#[cfg(not(windows))]
fn ffplay(path: &Path) -> Command {
    let mut command = Command::new("ffplay");
    command
        .args(["-nodisp", "-autoexit", "-loglevel", "quiet"])
        .arg(path);
    command
}
