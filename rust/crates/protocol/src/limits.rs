//! Every size limit in one place. The daemon parses bytes written by *any* process that can reach
//! its socket, so nothing is unbounded.

/// First two bytes of every frame.
pub const MAGIC: [u8; 2] = *b"LW";
/// Protocol version carried in every frame header.
pub const VERSION: u8 = 1;
/// Frame header: magic(2) + version(1) + kind(1) + body length(2, big endian).
pub const HEADER_LEN: usize = 6;
/// Largest accepted request body.
pub const MAX_REQUEST_BODY: usize = 8 * 1024;
/// Largest accepted response body.
pub const MAX_RESPONSE_BODY: usize = 1024;
/// Provider / instance names: `[A-Za-z0-9_-]{1,32}`.
pub const MAX_NAME_LEN: usize = 32;
/// Provider URLs.
pub const MAX_URL_LEN: usize = 2048;
/// User-Agent override.
pub const MAX_USER_AGENT_LEN: usize = 512;
/// Human readable text in a response.
pub const MAX_MESSAGE_LEN: usize = 512;
/// Idle timeout upper bound: one year, in minutes.
pub const MAX_IDLE_TIMEOUT_MINUTES: u32 = 525_600;
