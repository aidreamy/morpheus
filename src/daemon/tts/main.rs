//! The `morpheus-ttsd` binary.
//!
//! A thin shim over [`morpheus::daemon::tts::run`]: bind, serve, idle-exit.

/// The daemon's transport is a Unix domain socket; the binary exists only on
/// Unix (where `morpheus::daemon::tts` is compiled).
#[cfg(unix)]
fn main() {
    let code = morpheus::daemon::tts::run();
    std::process::exit(code);
}

#[cfg(not(unix))]
fn main() {
    eprintln!("morpheus-ttsd: the voice daemon is only available on Unix");
    std::process::exit(1);
}
