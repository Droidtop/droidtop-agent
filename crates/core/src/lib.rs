//! The part of droidtop-agent that both ends run: the computer's agent and
//! droidtop on the handheld (through `droidtop-agent-android`). One
//! implementation of the identity, pairing, session channel, protocol and
//! sync rules, so the two ends cannot drift apart (docs/DESIGN.md).
//!
//! Blocking I/O on purpose: a sync is a handful of request/reply exchanges,
//! the agent serves each connection on its own thread, and droidtop calls in
//! from a background coroutine. No async runtime to carry on the handheld.

pub mod channel;
pub mod context;
pub mod discovery;
pub mod error;
pub mod frame;
pub mod hex;
pub mod keys;
pub mod library;
pub mod mailbox;
pub mod manifest;
pub mod pairing;
pub mod proto;
pub mod saves;
pub mod savesync;
pub mod server;

pub use error::{Error, Result};

/// The agent's TCP port (sessions) and UDP port (discovery) on the LAN.
pub const PORT: u16 = 47610;

/// The protocol version both sides state in `hello`.
pub const PROTOCOL_VERSION: u32 = 1;

/// What this build of the core can do, stated in `hello`.
pub const FEATURES: &[&str] = &["saves", "library", "contexts"];
