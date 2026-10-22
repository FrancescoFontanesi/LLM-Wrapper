//! Daemon replies. The daemon answers *after* it acted on the request, so the status is the truth.

use crate::error::ProtocolError;
use crate::frame::{self, Kind};
use crate::limits::MAX_MESSAGE_LEN;

/// Outcome of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Done.
    Ok = 0,
    /// The request was well formed but refused (unknown provider, bad value, ...); see the message.
    Rejected = 1,
    /// The daemon does not understand this protocol version.
    Unsupported = 2,
    /// The daemon is busy; try again.
    Busy = 3,
    /// Something failed inside the daemon.
    Internal = 4,
}

impl Status {
    fn from_byte(b: u8) -> Result<Self, ProtocolError> {
        Ok(match b {
            0 => Self::Ok,
            1 => Self::Rejected,
            2 => Self::Unsupported,
            3 => Self::Busy,
            4 => Self::Internal,
            other => return Err(ProtocolError::UnknownStatus(other)),
        })
    }
}

/// A daemon reply: status byte plus an optional human-readable message (UTF-8, at most 512 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// Outcome.
    pub status: Status,
    /// Text for the user; empty on success.
    pub message: String,
}

impl Response {
    /// `Ok` with no message.
    pub fn ok() -> Self {
        Self {
            status: Status::Ok,
            message: String::new(),
        }
    }

    /// Any status with a message (truncated on a character boundary to the limit).
    pub fn with_message(status: Status, message: &str) -> Self {
        let mut end = message.len().min(MAX_MESSAGE_LEN);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            status,
            message: message[..end].to_owned(),
        }
    }

    /// Encode as a complete frame.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.message.len() > MAX_MESSAGE_LEN {
            return Err(ProtocolError::MessageTooLong);
        }
        let mut body = Vec::with_capacity(1 + self.message.len());
        body.push(self.status as u8);
        body.extend_from_slice(self.message.as_bytes());
        frame::encode(Kind::Response, &body)
    }

    /// Decode a frame body.
    pub fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        let (&status, text) = body.split_first().ok_or(ProtocolError::Truncated)?;
        let status = Status::from_byte(status)?;
        if text.len() > MAX_MESSAGE_LEN {
            return Err(ProtocolError::MessageTooLong);
        }
        let message = std::str::from_utf8(text)
            .map_err(|_| ProtocolError::InvalidUtf8(0))?
            .to_owned();
        Ok(Self { status, message })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_golden_bytes() {
        let r = Response::with_message(Status::Rejected, "unknown provider: x");
        let f = r.encode().unwrap();
        assert_eq!(&f[..7], &[b'L', b'W', 1, 2, 0, 20, 1]);
        assert_eq!(Response::decode(&f[6..]).unwrap(), r);
        assert_eq!(
            Response::ok().encode().unwrap(),
            [b'L', b'W', 1, 2, 0, 1, 0]
        );
    }

    #[test]
    fn rejects_malformed_bodies() {
        assert_eq!(Response::decode(&[]), Err(ProtocolError::Truncated));
        assert_eq!(Response::decode(&[9]), Err(ProtocolError::UnknownStatus(9)));
        assert_eq!(
            Response::decode(&[0, 0xff, 0xfe]),
            Err(ProtocolError::InvalidUtf8(0))
        );
        let mut long = vec![0_u8];
        long.extend(vec![b'a'; MAX_MESSAGE_LEN + 1]);
        assert_eq!(Response::decode(&long), Err(ProtocolError::MessageTooLong));
    }

    #[test]
    fn with_message_truncates_on_a_char_boundary() {
        let s = "\u{e9}".repeat(400);
        let r = Response::with_message(Status::Internal, &s);
        assert!(r.message.len() <= MAX_MESSAGE_LEN);
        assert!(r.encode().is_ok());
    }
}
