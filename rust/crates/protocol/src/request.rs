//! The request a client sends: a set of action flags plus optional typed arguments.
//!
//! Wire body = a sequence of fields `tag(1) | len(2, big endian) | value`, in *strictly ascending*
//! tag order. That makes the encoding canonical: for any byte string, at most one `Request`
//! decodes from it, and `encode(decode(b)) == b`. Unknown tags, repeats, reordering and trailing
//! bytes are all errors rather than being skipped.

use crate::error::ProtocolError;
use crate::frame::{self, Kind};
use crate::limits::MAX_IDLE_TIMEOUT_MINUTES;
use crate::name::ProviderName;
use crate::text::UserAgent;
use crate::url::ProviderUrl;

const TAG_FLAGS: u8 = 1;
const TAG_IDLE_TIMEOUT: u8 = 2;
const TAG_USER_AGENT: u8 = 3;
const TAG_PROVIDER: u8 = 4;
const TAG_ADD_NAME: u8 = 5;
const TAG_ADD_URL: u8 = 6;
const TAG_REMOVE: u8 = 7;

/// Action flags, one bit each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags(u16);

impl Flags {
    /// `--toggle`
    pub const TOGGLE: Self = Self(1 << 0);
    /// `--show`
    pub const SHOW: Self = Self(1 << 1);
    /// `--hide`
    pub const HIDE: Self = Self(1 << 2);
    /// `--new-chat`
    pub const NEW_CHAT: Self = Self(1 << 3);
    /// `--soft-clean`
    pub const SOFT_CLEAN: Self = Self(1 << 4);
    /// `--hard-clean`
    pub const HARD_CLEAN: Self = Self(1 << 5);
    /// `--background`
    pub const BACKGROUND: Self = Self(1 << 6);
    /// `--quit`
    pub const QUIT: Self = Self(1 << 7);

    const KNOWN: u16 = 0x00ff;

    /// No flags.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Raw bits.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Build from raw bits; `None` if any bit is undefined.
    pub const fn from_bits(bits: u16) -> Option<Self> {
        if bits & !Self::KNOWN == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    /// True if every bit of `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Set the bits of `other`.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

/// `--provider NAME|URL`
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderRef {
    /// A provider from the list.
    Name(ProviderName),
    /// An ad-hoc URL.
    Url(ProviderUrl),
}

/// Everything a client can ask the daemon to do. Cheap to build, impossible to build invalid.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// Action flags.
    pub flags: Flags,
    /// `--idle-timeout`, in minutes (1..=525600).
    pub idle_timeout_minutes: Option<u32>,
    /// `--user-agent`
    pub user_agent: Option<UserAgent>,
    /// `--provider`
    pub provider: Option<ProviderRef>,
    /// `--add-provider NAME URL`
    pub add_provider: Option<(ProviderName, ProviderUrl)>,
    /// `--remove-provider NAME`
    pub remove_provider: Option<ProviderName>,
}

fn put(out: &mut Vec<u8>, tag: u8, value: &[u8]) -> Result<(), ProtocolError> {
    let len = u16::try_from(value.len()).map_err(|_| ProtocolError::BadFieldLength(tag))?;
    out.push(tag);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}

impl Request {
    /// Canonical body bytes (without the frame header).
    pub fn encode_body(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut out = Vec::new();
        put(&mut out, TAG_FLAGS, &self.flags.bits().to_be_bytes())?;
        if let Some(m) = self.idle_timeout_minutes {
            if !(1..=MAX_IDLE_TIMEOUT_MINUTES).contains(&m) {
                return Err(ProtocolError::BadFieldLength(TAG_IDLE_TIMEOUT));
            }
            put(&mut out, TAG_IDLE_TIMEOUT, &m.to_be_bytes())?;
        }
        if let Some(ua) = &self.user_agent {
            put(&mut out, TAG_USER_AGENT, ua.as_str().as_bytes())?;
        }
        if let Some(p) = &self.provider {
            let (kind, text) = match p {
                ProviderRef::Name(n) => (0_u8, n.as_str()),
                ProviderRef::Url(u) => (1_u8, u.as_str()),
            };
            let mut v = vec![kind];
            v.extend_from_slice(text.as_bytes());
            put(&mut out, TAG_PROVIDER, &v)?;
        }
        if let Some((name, url)) = &self.add_provider {
            put(&mut out, TAG_ADD_NAME, name.as_str().as_bytes())?;
            put(&mut out, TAG_ADD_URL, url.as_str().as_bytes())?;
        }
        if let Some(name) = &self.remove_provider {
            put(&mut out, TAG_REMOVE, name.as_str().as_bytes())?;
        }
        Ok(out)
    }

    /// Complete frame, ready to write to the socket.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        frame::encode(Kind::Request, &self.encode_body()?)
    }

    /// Decode and fully validate a frame body. Every value is validated again here: the daemon
    /// never trusts that a client ran its own checks.
    pub fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        let mut req = Self::default();
        let mut have_flags = false;
        let mut add_name: Option<ProviderName> = None;
        let mut add_url: Option<ProviderUrl> = None;
        let mut last_tag = 0_u8;
        let mut rest = body;

        while !rest.is_empty() {
            let (head, after) = rest.split_at_checked(3).ok_or(ProtocolError::Truncated)?;
            let tag = head[0];
            let len = usize::from(u16::from_be_bytes([head[1], head[2]]));
            let (value, after) = after
                .split_at_checked(len)
                .ok_or(ProtocolError::Truncated)?;
            rest = after;
            if tag <= last_tag {
                return Err(ProtocolError::DuplicateOrUnordered(tag));
            }
            last_tag = tag;
            match tag {
                TAG_FLAGS => {
                    let bytes: [u8; 2] = value
                        .try_into()
                        .map_err(|_| ProtocolError::BadFieldLength(tag))?;
                    let bits = u16::from_be_bytes(bytes);
                    req.flags = Flags::from_bits(bits).ok_or(ProtocolError::UnknownFlags(bits))?;
                    have_flags = true;
                }
                TAG_IDLE_TIMEOUT => {
                    let bytes: [u8; 4] = value
                        .try_into()
                        .map_err(|_| ProtocolError::BadFieldLength(tag))?;
                    let m = u32::from_be_bytes(bytes);
                    if !(1..=MAX_IDLE_TIMEOUT_MINUTES).contains(&m) {
                        return Err(ProtocolError::BadFieldLength(tag));
                    }
                    req.idle_timeout_minutes = Some(m);
                }
                TAG_USER_AGENT => req.user_agent = Some(UserAgent::parse(utf8(tag, value)?)?),
                TAG_PROVIDER => {
                    let (&kind, text) = value
                        .split_first()
                        .ok_or(ProtocolError::BadFieldLength(tag))?;
                    let text = utf8(tag, text)?;
                    req.provider = Some(match kind {
                        0 => ProviderRef::Name(ProviderName::parse(text)?),
                        1 => ProviderRef::Url(ProviderUrl::parse(text)?),
                        _ => return Err(ProtocolError::BadFieldLength(tag)),
                    });
                }
                TAG_ADD_NAME => add_name = Some(ProviderName::parse(utf8(tag, value)?)?),
                TAG_ADD_URL => add_url = Some(ProviderUrl::parse(utf8(tag, value)?)?),
                TAG_REMOVE => req.remove_provider = Some(ProviderName::parse(utf8(tag, value)?)?),
                other => return Err(ProtocolError::UnknownTag(other)),
            }
        }

        if !have_flags {
            return Err(ProtocolError::MissingFlags);
        }
        req.add_provider = match (add_name, add_url) {
            (Some(n), Some(u)) => Some((n, u)),
            (None, None) => None,
            _ => return Err(ProtocolError::IncompleteAddProvider),
        };
        Ok(req)
    }
}

fn utf8(tag: u8, bytes: &[u8]) -> Result<&str, ProtocolError> {
    std::str::from_utf8(bytes).map_err(|_| ProtocolError::InvalidUtf8(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> Request {
        Request {
            flags: {
                let mut f = Flags::TOGGLE;
                f.insert(Flags::NEW_CHAT);
                f
            },
            idle_timeout_minutes: Some(30),
            user_agent: Some(UserAgent::parse("UA/1").unwrap()),
            provider: Some(ProviderRef::Url(
                ProviderUrl::parse("https://x.example/").unwrap(),
            )),
            add_provider: Some((
                ProviderName::parse("mistral").unwrap(),
                ProviderUrl::parse("https://chat.mistral.ai").unwrap(),
            )),
            remove_provider: Some(ProviderName::parse("claude").unwrap()),
        }
    }

    #[test]
    fn round_trip() {
        for r in [Request::default(), full()] {
            let body = r.encode_body().unwrap();
            assert_eq!(Request::decode(&body).unwrap(), r);
        }
    }

    #[test]
    fn flags_are_strict() {
        assert!(Flags::from_bits(0x00ff).is_some());
        assert!(Flags::from_bits(0x0100).is_none());
        assert!(Flags::TOGGLE.contains(Flags::TOGGLE));
        assert!(!Flags::TOGGLE.contains(Flags::SHOW));
        assert_eq!(
            Request::decode(&[1, 0, 2, 0x01, 0x00]),
            Err(ProtocolError::UnknownFlags(0x0100))
        );
    }

    #[test]
    fn decoding_is_canonical() {
        let body = full().encode_body().unwrap();
        // trailing byte
        let mut t = body.clone();
        t.push(0);
        assert_eq!(Request::decode(&t), Err(ProtocolError::Truncated));
        // duplicated flags field
        let mut d = body.clone();
        d.extend_from_slice(&[1, 0, 2, 0, 0]);
        assert_eq!(
            Request::decode(&d),
            Err(ProtocolError::DuplicateOrUnordered(1))
        );
        // out of order: user agent before flags
        let swapped = [&[3_u8, 0, 1, b'a'][..], &[1, 0, 2, 0, 0]].concat();
        assert_eq!(
            Request::decode(&swapped),
            Err(ProtocolError::DuplicateOrUnordered(1))
        );
        // unknown tag
        assert_eq!(
            Request::decode(&[1, 0, 2, 0, 0, 99, 0, 0]),
            Err(ProtocolError::UnknownTag(99))
        );
        // no flags
        assert_eq!(Request::decode(&[]), Err(ProtocolError::MissingFlags));
        assert_eq!(
            Request::decode(&[3, 0, 1, b'a']),
            Err(ProtocolError::MissingFlags)
        );
        // add-provider halves
        assert_eq!(
            Request::decode(&[1, 0, 2, 0, 0, 5, 0, 1, b'a']),
            Err(ProtocolError::IncompleteAddProvider)
        );
    }

    #[test]
    fn decode_revalidates_every_value() {
        let with = |tag: u8, v: &[u8]| {
            let mut b = vec![1, 0, 2, 0, 0, tag];
            b.extend_from_slice(&u16::try_from(v.len()).unwrap().to_be_bytes());
            b.extend_from_slice(v);
            b
        };
        assert!(matches!(
            Request::decode(&with(3, b"bad\r\nua")),
            Err(ProtocolError::Invalid(_))
        ));
        assert!(matches!(
            Request::decode(&with(7, b"bad name")),
            Err(ProtocolError::Invalid(_))
        ));
        assert!(matches!(
            Request::decode(&with(4, b"\x01javascript:alert(1)")),
            Err(ProtocolError::Invalid(_))
        ));
        assert_eq!(
            Request::decode(&with(4, b"\x02x")),
            Err(ProtocolError::BadFieldLength(4))
        );
        assert_eq!(
            Request::decode(&with(4, b"")),
            Err(ProtocolError::BadFieldLength(4))
        );
        assert_eq!(
            Request::decode(&with(3, &[0xff, 0xfe])),
            Err(ProtocolError::InvalidUtf8(3))
        );
        assert_eq!(
            Request::decode(&with(2, &[0, 0, 0, 0])),
            Err(ProtocolError::BadFieldLength(2))
        );
        assert_eq!(
            Request::decode(&with(2, &[0, 0, 0])),
            Err(ProtocolError::BadFieldLength(2))
        );
    }

    #[test]
    fn a_field_longer_than_the_buffer_is_truncation_not_a_panic() {
        assert_eq!(
            Request::decode(&[1, 0xff, 0xff, 0]),
            Err(ProtocolError::Truncated)
        );
        assert_eq!(Request::decode(&[1, 0]), Err(ProtocolError::Truncated));
    }
}
