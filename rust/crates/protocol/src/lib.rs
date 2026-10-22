//! Wire protocol, input validation and command-line grammar of llm-wrapper.
//!
//! Shared by the `llm-wrapper` client and the `llm-wrapperd` daemon so that both ends interpret
//! every byte and every argument identically. The crate has no dependencies, performs no I/O beyond
//! the `Read` it is handed, forbids `unsafe`, and treats all input as hostile: sizes are bounded,
//! encodings are canonical, and every accepted value has been validated (again) on the receiving
//! side.

pub mod cli;
pub mod error;
pub mod frame;
pub mod limits;
pub mod name;
pub mod request;
pub mod response;
pub mod text;
pub mod url;

pub use cli::{parse as parse_args, CliError, Invocation, Parsed, USAGE};
pub use error::{ProtocolError, ValidationError};
pub use name::{InstanceName, ProviderName};
pub use request::{Flags, ProviderRef, Request};
pub use response::{Response, Status};
pub use text::{sanitize_for_terminal, UserAgent};
pub use url::ProviderUrl;
