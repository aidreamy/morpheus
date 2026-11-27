pub const KEYRING_SERVICE: &str = "morpheus";

pub mod agent_profiles;
pub mod assembly;
pub mod config;
pub mod coordinator;
pub mod daemon;
pub mod fallback;
pub mod git;
pub mod harness;
pub mod mcp;
pub mod notification;
pub mod session;
pub mod setup;
/// Local voice output (Kokoro TTS through sherpa-onnx). Gated behind the
/// `speak` feature: sherpa-onnx statically links ONNX Runtime.
#[cfg(feature = "speak")]
pub mod speak;
pub mod telemetry;
pub mod types;
pub mod uninstall;
pub mod update;
pub mod usage;
pub mod util;
/// The weights installer's application side (config → install → events).
/// Only meaningful with the ONNX engine: without the feature the decision
/// model cannot load, so the module compiles empty.
#[cfg(feature = "onnx")]
pub mod weights;
pub mod whatsapp;
pub use harness::events::ModelEntry;
pub use morpheus_tools::{bash, fs, plan, skills, web};
