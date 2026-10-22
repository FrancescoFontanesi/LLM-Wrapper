//! Black-box end-to-end suite for llm-wrapper (Phase 0).
//!
//! It drives an implementation only through its public surface — command line, state file,
//! socket, window class/keys on X11, and files on disk — so the same tests run against the legacy
//! Python app today and against the Rust client + daemon as they land. `docs/spec.md` explains
//! what each group of tests pins down.

pub mod fakechat;
pub mod proc;
pub mod sandbox;

pub use fakechat::FakeChat;
pub use sandbox::{known_defect, Impl, Out, Sandbox};
