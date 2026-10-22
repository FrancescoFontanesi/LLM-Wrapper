//! Platform pieces shared by the `llm-wrapper` client and the `llm-wrapperd` daemon.
//!
//! * [`runtime`] — where the socket lives and the checks that make the runtime directory and the
//!   socket safe to trust.
//! * [`private`] — creating, reading and atomically writing files that only the user can reach.
//! * [`client`] — the IPC exchange with a running daemon, under hard deadlines.
//!
//! No dependencies besides the protocol crate, no `unsafe`.

pub mod client;
pub mod private;
pub mod runtime;
