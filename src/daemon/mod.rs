//! Daemons — out-of-process services shared by every morpheus instance.
//!
//! The app is assembled per turn and is routinely open in several instances
//! at once; any expensive resident resource (a ~1.3 GB ONNX graph and its
//! ORT session, today) duplicated per process is a waste. A daemon owns the
//! resource once and every morpheus process reaches it over the OS's local IPC.
//!
//! ## Layout
//!
//! ```text
//! src/daemon/
//!   mod.rs         this file: the architecture contract for a new daemon
//!   ipc.rs         transport: UDS listen/connect, length-prefixed JSON-RPC 2.0
//!                  framing, per-connection request loop
//!   activation.rs  lifecycle: socket activation (spawn-on-demand), the
//!                  bind-as-lock race resolution, the idle shutdown timer
//!   client.rs      the generic client handle (connect, request/response
//!                  correlation, reconnect); protocol types stay per-daemon
//!   decision/      the decision-model daemon (`morpheus-decisiond`): protocol,
//!                  server, resident model registry, and the `Checkup` client
//!   tts/           the voice daemon (`morpheus-ttsd`): protocol, server,
//!                  resident TTS model, and the synthesis client
//! ```
//!
//! ## How to add a new daemon
//!
//! One domain, one subdirectory, one binary, one socket:
//!
//! 1. Create `src/daemon/<name>/` with `protocol.rs` (a `Params` enum
//!    tagged by method — see `decision/protocol.rs`), `server.rs` (routing
//!    for that protocol), and `client.rs` (the seam-side adapter).
//! 2. Register `[[bin]] name = "morpheus-<name>d",
//!    path = "src/daemon/<name>/main.rs"` in `Cargo.toml`.
//! 3. Activate through `daemon::activation`: the socket path is derived from
//!    the daemon name; nothing else in the shared layer changes.
//!
//! The shared layer is app-internal by design (only morpheus consumes it). If a
//! second workspace crate ever needs a daemon, `ipc.rs`/`activation.rs`/
//! `client.rs` are the exact boundary to lift into a `morpheus-ipc` crate.
//!
//! The shared layer is gated on the features of the daemons it carries: it
//! compiles when either the decision engine (`onnx`) or the voice engine
//! (`speak`) is enabled, and each daemon subdirectory keeps its own gate. A
//! build with neither feature compiles none of this (no dead code, no
//! warnings — the module tree IS the feature boundary).
#![cfg(any(feature = "onnx", all(feature = "speak", unix)))]

pub(crate) mod activation;
pub(crate) mod client;
pub(crate) mod ipc;
pub(crate) mod ipc_error;

/// The decision daemon (see the module docs above for the layout and the
/// "how to add one" contract).
#[cfg(feature = "onnx")]
pub mod decision;

/// The voice daemon. Unix-only: the transport is a Unix domain socket.
#[cfg(all(feature = "speak", unix))]
pub mod tts;
