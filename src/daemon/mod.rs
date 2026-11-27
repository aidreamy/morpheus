//! Daemons - out-of-process services shared by every morpheus instance.
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
//!   transport.rs   the one platform seam: local stream sockets (Unix domain
//!                  sockets on Unix, AF_UNIX on Windows)
//!   ipc.rs         the protocol: listen/connect, length-prefixed JSON-RPC 2.0
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
//!    tagged by method - see `decision/protocol.rs`), `server.rs` (routing
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
//! warnings - the module tree IS the feature boundary).
//!
//! ## Platforms
//!
//! The shared layer is OS-independent apart from [`transport`]: the socket
//! is a Unix domain socket on Unix and an AF_UNIX socket on Windows
//! (Windows 10 1803+ / Server 2019+), so the protocol, the framing, the
//! timeouts and the activation flow are identical on both. The Windows
//! activation lock is `LockFileEx`, the `flock` equivalent.
#![cfg(any(feature = "onnx", feature = "speak"))]

pub(crate) mod activation;
pub(crate) mod client;
pub(crate) mod ipc;
pub(crate) mod ipc_error;
pub(crate) mod transport;

/// The decision daemon (see the module docs above for the layout and the
/// "how to add one" contract).
#[cfg(feature = "onnx")]
pub mod decision;

/// The voice daemon: synthesis behind the same cross-platform transport.
#[cfg(feature = "speak")]
pub mod tts;
