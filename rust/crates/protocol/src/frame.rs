//! Frame layer: `magic(2) | version(1) | kind(1) | body_len(2, big endian) | body`.

use std::io::Read;

use crate::error::ProtocolError;
use crate::limits::{HEADER_LEN, MAGIC, MAX_REQUEST_BODY, MAX_RESPONSE_BODY, VERSION};

/// What a frame carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Client to daemon.
    Request = 1,
    /// Daemon to client.
    Response = 2,
}

impl Kind {
    /// Largest body accepted for this kind.
    pub const fn max_body(self) -> usize {
        match self {
            Kind::Request => MAX_REQUEST_BODY,
            Kind::Response => MAX_RESPONSE_BODY,
        }
    }
}

/// Wrap `body` in a frame header.
pub fn encode(kind: Kind, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let max = kind.max_body();
    if body.len() > max {
        return Err(ProtocolError::FrameTooLarge {
            len: body.len(),
            max,
        });
    }
    let len = u16::try_from(body.len()).map_err(|_| ProtocolError::FrameTooLarge {
        len: body.len(),
        max,
    })?;
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(kind as u8);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

/// Validate a header and return the declared body length. Checked in this order so the error names
/// the first thing that is wrong: magic, version, kind, size.
pub fn parse_header(header: &[u8; HEADER_LEN], expected: Kind) -> Result<usize, ProtocolError> {
    if header[..2] != MAGIC {
        return Err(ProtocolError::BadMagic);
    }
    if header[2] != VERSION {
        return Err(ProtocolError::UnsupportedVersion(header[2]));
    }
    if header[3] != expected as u8 {
        return Err(ProtocolError::UnexpectedKind(header[3]));
    }
    let len = usize::from(u16::from_be_bytes([header[4], header[5]]));
    let max = expected.max_body();
    if len > max {
        return Err(ProtocolError::FrameTooLarge { len, max });
    }
    Ok(len)
}

/// Read exactly one frame of the `expected` kind. Never reads more than `HEADER_LEN` plus the
/// kind's maximum body, whatever the peer sends.
pub fn read<R: Read>(reader: &mut R, expected: Kind) -> Result<Vec<u8>, ProtocolError> {
    let mut header = [0_u8; HEADER_LEN];
    reader.read_exact(&mut header)?;
    let len = parse_header(&header, expected)?;
    let mut body = vec![0_u8; len];
    reader.read_exact(&mut body)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_bytes_are_exactly_as_documented() {
        let f = encode(Kind::Request, &[0xaa, 0xbb, 0xcc]).unwrap();
        assert_eq!(f, [b'L', b'W', 1, 1, 0, 3, 0xaa, 0xbb, 0xcc]);
        let f = encode(Kind::Response, &[]).unwrap();
        assert_eq!(f, [b'L', b'W', 1, 2, 0, 0]);
    }

    #[test]
    fn rejects_in_documented_order() {
        let h = |a: [u8; 6]| parse_header(&a, Kind::Request);
        assert_eq!(h([b'X', b'W', 1, 1, 0, 0]), Err(ProtocolError::BadMagic));
        assert_eq!(
            h([b'L', b'W', 2, 1, 0, 0]),
            Err(ProtocolError::UnsupportedVersion(2))
        );
        assert_eq!(
            h([b'L', b'W', 1, 2, 0, 0]),
            Err(ProtocolError::UnexpectedKind(2))
        );
        assert_eq!(
            h([b'L', b'W', 1, 9, 0, 0]),
            Err(ProtocolError::UnexpectedKind(9))
        );
        assert_eq!(
            h([b'L', b'W', 1, 1, 0xff, 0xff]),
            Err(ProtocolError::FrameTooLarge {
                len: 65535,
                max: MAX_REQUEST_BODY
            })
        );
        assert_eq!(h([b'L', b'W', 1, 1, 0x20, 0x00]), Ok(MAX_REQUEST_BODY));
        assert!(h([b'L', b'W', 1, 1, 0x20, 0x01]).is_err());
    }

    #[test]
    fn response_limit_is_tighter() {
        assert!(encode(Kind::Response, &vec![0; MAX_RESPONSE_BODY]).is_ok());
        assert!(encode(Kind::Response, &vec![0; MAX_RESPONSE_BODY + 1]).is_err());
        assert!(encode(Kind::Request, &vec![0; MAX_REQUEST_BODY + 1]).is_err());
    }

    #[test]
    fn read_never_goes_beyond_the_declared_body() {
        let mut data = encode(Kind::Request, b"abc").unwrap();
        data.extend_from_slice(b"TRAILING GARBAGE");
        let mut cur = std::io::Cursor::new(data);
        assert_eq!(read(&mut cur, Kind::Request).unwrap(), b"abc");
        assert_eq!(cur.position(), 9);
    }

    #[test]
    fn truncation_is_an_error_at_every_length() {
        let full = encode(Kind::Request, b"abcdef").unwrap();
        for cut in 0..full.len() {
            let mut cur = std::io::Cursor::new(&full[..cut]);
            assert_eq!(
                read(&mut cur, Kind::Request),
                Err(ProtocolError::Truncated),
                "cut={cut}"
            );
        }
    }
}
