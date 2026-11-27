//! The `morpheus-ttsd` binary.
//!
//! A thin shim over [`morpheus::daemon::tts::run`]: bind, serve, idle-exit.

fn main() {
    let code = morpheus::daemon::tts::run();
    std::process::exit(code);
}
