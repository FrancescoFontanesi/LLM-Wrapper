//! Error types. All of them are plain data so they can be compared in tests and printed verbatim.

use std::fmt;
use std::io;

/// Why a name (provider or instance) was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameProblem {
    /// Zero length.
    Empty,
    /// More than 32 bytes.
    TooLong,
    /// Anything outside `[A-Za-z0-9_-]`.
    BadChar,
}

/// Why a provider URL was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlProblem {
    /// Zero length.
    Empty,
    /// Longer than 2048 bytes.
    TooLong,
    /// Whitespace, control characters, non-ASCII, or a character that is never legal in a URL.
    BadChar,
    /// No `://`.
    MissingScheme,
    /// Anything but `http` / `https`.
    UnsupportedScheme,
    /// `user:password@host`.
    Userinfo,
    /// Empty host.
    EmptyHost,
    /// Host is not a plain DNS name or canonical IP address.
    BadHost,
    /// Malformed `[IPv6]` literal.
    BadIpv6,
    /// Port is not 1-65535.
    BadPort,
    /// `%` not followed by two hex digits.
    BadPercent,
    /// `http` to anything but a loopback address.
    InsecureHttp,
}

/// Why a User-Agent override was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAgentProblem {
    /// Zero length.
    Empty,
    /// More than 512 bytes.
    TooLong,
    /// Not printable ASCII.
    BadChar,
}

/// Why an idle timeout was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleProblem {
    /// Not an optionally signed ASCII integer.
    NotANumber,
    /// Above one year.
    TooLarge,
}

/// A value that failed validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationError {
    /// Provider name.
    ProviderName(NameProblem),
    /// Instance name.
    InstanceName(NameProblem),
    /// Provider URL.
    Url(UrlProblem),
    /// User-Agent override.
    UserAgent(UserAgentProblem),
    /// Idle timeout.
    IdleTimeout(IdleProblem),
}

impl fmt::Display for NameProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "must not be empty",
            Self::TooLong => "must be at most 32 characters",
            Self::BadChar => "may only contain letters, digits, '_' and '-'",
        })
    }
}

impl fmt::Display for UrlProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "must not be empty",
            Self::TooLong => "must be at most 2048 characters",
            Self::BadChar => {
                "contains whitespace, control or non-ASCII characters (use punycode for IDN hosts)"
            }
            Self::MissingScheme => "must look like https://host/",
            Self::UnsupportedScheme => "scheme must be https (or http for a loopback address)",
            Self::Userinfo => "must not contain credentials (user:password@)",
            Self::EmptyHost => "has no host",
            Self::BadHost => "host must be a DNS name or a canonical IP address",
            Self::BadIpv6 => "malformed IPv6 address",
            Self::BadPort => "port must be between 1 and 65535",
            Self::BadPercent => "has a '%' that is not followed by two hex digits",
            Self::InsecureHttp => {
                "plain http is only allowed for localhost / loopback addresses; use https"
            }
        })
    }
}

impl fmt::Display for UserAgentProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "must not be empty",
            Self::TooLong => "must be at most 512 characters",
            Self::BadChar => "may only contain printable ASCII",
        })
    }
}

impl fmt::Display for IdleProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotANumber => "must be a whole number of minutes",
            Self::TooLarge => "must be at most 525600 minutes (one year)",
        })
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderName(p) => write!(f, "invalid provider name: {p}"),
            Self::InstanceName(p) => write!(f, "invalid instance name: {p}"),
            Self::Url(p) => write!(f, "invalid URL: {p}"),
            Self::UserAgent(p) => write!(f, "invalid user agent: {p}"),
            Self::IdleTimeout(p) => write!(f, "invalid idle timeout: {p}"),
        }
    }
}

impl std::error::Error for ValidationError {}

/// A frame or message that is not valid protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// Header does not start with `LW`.
    BadMagic,
    /// Header carries a version this build does not speak.
    UnsupportedVersion(u8),
    /// Header carries the wrong message kind.
    UnexpectedKind(u8),
    /// Declared body is larger than the limit for that kind.
    FrameTooLarge {
        /// Declared length.
        len: usize,
        /// Limit.
        max: usize,
    },
    /// Input ended early.
    Truncated,
    /// Field tag this version does not define.
    UnknownTag(u8),
    /// Tags must be strictly ascending (canonical form); a repeat or reordering is an error.
    DuplicateOrUnordered(u8),
    /// Field has the wrong length for its tag.
    BadFieldLength(u8),
    /// The mandatory flags field is absent.
    MissingFlags,
    /// Flag bits this version does not define.
    UnknownFlags(u16),
    /// `add-provider` needs both a name and a URL.
    IncompleteAddProvider,
    /// Field that must be UTF-8 is not.
    InvalidUtf8(u8),
    /// Field decoded fine but failed validation.
    Invalid(ValidationError),
    /// Response status byte this version does not define.
    UnknownStatus(u8),
    /// Response message above the limit.
    MessageTooLong,
    /// Transport error while reading or writing a frame.
    Io(io::ErrorKind),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => f.write_str("not an llm-wrapper message (bad magic)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported protocol version {v}"),
            Self::UnexpectedKind(k) => write!(f, "unexpected message kind {k}"),
            Self::FrameTooLarge { len, max } => {
                write!(f, "message of {len} bytes exceeds the {max} byte limit")
            }
            Self::Truncated => f.write_str("message ended unexpectedly"),
            Self::UnknownTag(t) => write!(f, "unknown field tag {t}"),
            Self::DuplicateOrUnordered(t) => write!(f, "field {t} is repeated or out of order"),
            Self::BadFieldLength(t) => write!(f, "field {t} has an invalid length"),
            Self::MissingFlags => f.write_str("request has no flags field"),
            Self::UnknownFlags(b) => write!(f, "unknown flag bits {b:#06x}"),
            Self::IncompleteAddProvider => f.write_str("add-provider needs both a name and a URL"),
            Self::InvalidUtf8(t) => write!(f, "field {t} is not valid UTF-8"),
            Self::Invalid(e) => e.fmt(f),
            Self::UnknownStatus(s) => write!(f, "unknown status {s}"),
            Self::MessageTooLong => f.write_str("response message is too long"),
            Self::Io(k) => write!(f, "I/O error: {k}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<ValidationError> for ProtocolError {
    fn from(e: ValidationError) -> Self {
        Self::Invalid(e)
    }
}

impl From<io::Error> for ProtocolError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Self::Truncated
        } else {
            Self::Io(e.kind())
        }
    }
}
