//! The `llm-wrapper` command-line client.
//!
//! Talks to a running daemon over a private unix socket, or starts one. It has no dependencies
//! beyond the shared protocol crate and the standard library, contains no `unsafe`, and verifies
//! the runtime directory, the socket and the daemon binary before trusting any of them.

pub mod app;
pub mod client;
pub mod daemon;
pub mod runtime;
