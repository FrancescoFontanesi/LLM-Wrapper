//! Provider and instance names: `[A-Za-z0-9_-]{1,32}`, ASCII only.

use std::fmt;
use std::str::FromStr;

use crate::error::{NameProblem, ValidationError};
use crate::limits::MAX_NAME_LEN;

fn check(s: &str) -> Result<(), NameProblem> {
    if s.is_empty() {
        Err(NameProblem::Empty)
    } else if s.len() > MAX_NAME_LEN {
        Err(NameProblem::TooLong)
    } else if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Err(NameProblem::BadChar)
    } else {
        Ok(())
    }
}

macro_rules! name_type {
    ($(#[$meta:meta])* $ty:ident, $variant:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $ty(String);

        impl $ty {
            /// Validate `s` and wrap it.
            pub fn parse(s: &str) -> Result<Self, ValidationError> {
                check(s).map_err(ValidationError::$variant)?;
                Ok(Self(s.to_owned()))
            }

            /// The validated text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $ty {
            type Err = ValidationError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::parse(s)
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(
    /// Name of a provider in the provider list (`chatgpt`, `gemini`, ...).
    ProviderName,
    ProviderName
);
name_type!(
    /// Name of a daemon instance (`--instance NAME`); selects socket, state file and data directories.
    InstanceName,
    InstanceName
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_legacy_grammar() {
        for ok in ["a", "chatgpt", "A_b-9", "-5", "_", &"x".repeat(32)] {
            assert!(ProviderName::parse(ok).is_ok(), "{ok}");
            assert!(InstanceName::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_everything_else() {
        assert_eq!(
            ProviderName::parse(""),
            Err(ValidationError::ProviderName(NameProblem::Empty))
        );
        assert_eq!(
            ProviderName::parse(&"x".repeat(33)),
            Err(ValidationError::ProviderName(NameProblem::TooLong))
        );
        for bad in [
            "a b",
            "a.b",
            "a/b",
            "a\n",
            "a\0",
            "\u{e9}",
            "a:b",
            "../x",
            "a\u{ff0d}b",
            "\u{0665}",
        ] {
            assert_eq!(
                InstanceName::parse(bad),
                Err(ValidationError::InstanceName(NameProblem::BadChar)),
                "{bad:?}"
            );
        }
    }
}
