#![allow(dead_code)]

mod agent_colors;
mod app;
// The command-line surface lives at `src/cli` (app level, not TUI level); the
// bin declares it with an explicit path because this file sits one directory
// down, and the help screen needs the TUI's theme registry.
#[path = "../cli/mod.rs"]
mod cli;
mod component;
mod keymap;
mod left_panel;
mod lsp_colors;
mod routes;
mod speech;
mod state;
mod theme;
mod transcript_export;
mod ui;
mod util;
mod wallpaper;
// Application modules that live in the library now (moved out of the TUI);
// aliased at the bin root so `crate::x` paths in the app keep resolving.
use morpheus::{config, fallback, notification, session, types, update, usage};

#[cfg(test)]
#[path = "test/rag_abort.rs"]
mod test;

use app::App;
use cli::style::Palette;
use dotenvy::dotenv;

#[tokio::main]
async fn main() {
    let invocation = match cli::parse(std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("morpheus: {error}");
            eprintln!("Run `morpheus --help` for usage.");
            std::process::exit(2);
        }
    };

    match invocation {
        cli::Invocation::Help(subject) => {
            print!(
                "{}",
                cli::help::render(subject.as_deref(), &Palette::load())
            );
        }
        cli::Invocation::Version => println!("{} {}", cli::NAME, cli::VERSION),
        cli::Invocation::Paths => {
            prepare_plain_command("cli_paths");
            std::process::exit(cli::paths::run(&Palette::load()));
        }
        cli::Invocation::WhatsApp => {
            prepare_plain_command("cli_whatsapp");
            std::process::exit(morpheus::whatsapp::run().await);
        }
        cli::Invocation::Update(options) => {
            prepare_plain_command("cli_update");
            std::process::exit(cli::update::run(options).await);
        }
        cli::Invocation::CheckKeyring => check_keyring(),
        cli::Invocation::Launch(options) => run_tui(options).await,
    }
}

/// Environment the non-TUI commands share. Logging is best effort: a log that
/// cannot be opened must never block an update or a path lookup.
fn prepare_plain_command(scope: &str) {
    dotenv().ok();
    if let Err(reason) = morpheus::util::logger::try_init(scope) {
        eprintln!("morpheus: logging disabled: {reason}");
    }
}

/// Start the interactive TUI.
async fn run_tui(options: cli::LaunchOptions) {
    dotenv().ok();

    // Initialize debug file logger.
    // Logs are written to /tmp/morpheus/log/stdout.log (shared with the MCP
    // server stderr logs in the same directory).
    morpheus::util::logger::init("tui_main");

    // Resolved BEFORE the terminal is initialized, so the messages are plain
    // stderr lines: an unknown `-s` id exits, an empty `-c` falls back to a
    // fresh session with a hint.
    let startup_session = match cli::resume::resolve(&options) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("morpheus: {error}");
            std::process::exit(1);
        }
    };

    crate::util::selection::init_clipboard();
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "?".to_string());
    let mut app = App::new(cwd);
    if let Some(session_id) = startup_session
        && !app.open_session_at_startup(&session_id)
    {
        eprintln!(
            "morpheus: session {session_id} is already open in another morpheus process — starting a new session."
        );
    }
    app.show_welcome_toast();
    app.show_telemetry_consent_prompt();
    if let Err(e) = app.run() {
        eprintln!("Error: {e}");
    }
    // Telemetry shutdown (AFTER the terminal is restored by `run`): enqueue
    // the session summary and flush the queue. Awaited so the bounded
    // upload cannot race process exit; no-op without consent or ingest
    // configuration.
    app.telemetry_shutdown().await;
}

/// Diagnostic for the OS credential store backing the API keys.
///
/// Read-only: reports, per provider, whether a key is available from the
/// environment and/or from the keyring, plus a write/read/delete roundtrip
/// through the native store to prove end-to-end connectivity.
fn check_keyring() {
    use aiface::connector::known_providers_with_env;
    use keyring::Entry;

    let mut rows: Vec<(String, bool, bool)> = Vec::new();
    let mut read_errors = 0usize;
    for (provider, env_var) in known_providers_with_env() {
        let env = std::env::var(env_var).is_ok();
        let keyring = match Entry::new(morpheus::KEYRING_SERVICE, env_var) {
            Ok(entry) => entry.get_password().is_ok(),
            Err(err) => {
                read_errors += 1;
                eprintln!("  keyring error for {provider}: {err}");
                false
            }
        };
        rows.push((provider.to_string(), env, keyring));
    }

    println!("morpheus keyring status");
    println!(
        "  dbus : {:?}",
        std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .ok()
            .unwrap_or_else(|| "<unset>".to_string())
    );
    println!(
        "  keys : {} stored under service \"{}\"",
        rows.iter().filter(|(_, _, kr)| *kr).count(),
        morpheus::KEYRING_SERVICE
    );
    println!();
    println!("{:<16} {:<6} {:<8} source", "provider", "env", "keyring");
    for (p, env, kr) in rows {
        let source = if kr {
            "keyring"
        } else if env {
            "env"
        } else {
            "-"
        };
        println!(
            "{p:<16} {:<6} {:<8} {source}",
            if env { "yes" } else { "no" },
            if kr { "yes" } else { "no" }
        );
    }

    // Write/read/delete roundtrip to prove the native store works end to end.
    let selftest_key = "__morpheus_selftest";
    let roundtrip = (|| -> Result<(), keyring::Error> {
        let entry = Entry::new(morpheus::KEYRING_SERVICE, selftest_key)?;
        entry.set_password("roundtrip-ok")?;
        let value = entry.get_password()?;
        let ok = value == "roundtrip-ok";
        entry.delete_credential()?;
        if ok {
            Ok(())
        } else {
            Err(keyring::Error::NoEntry)
        }
    })();
    println!();
    match roundtrip {
        Ok(()) => println!("roundtrip : OK (write/read/delete via the native OS store)"),
        Err(err) => {
            println!("roundtrip : FAILED: {err}");
            std::process::exit(2);
        }
    }
    if read_errors > 0 {
        std::process::exit(2);
    }
}
