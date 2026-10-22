//! Command-line grammar of `llm-wrapper` / `llm-wrapperd`, shared so both binaries parse identically.
//!
//! The grammar follows the legacy `argparse` definition where it matters (`--opt value`,
//! `--opt=value`, values that start with `-` only if they are negative numbers or contain a space,
//! the last occurrence of a repeated option wins) and deliberately differs where the legacy
//! behaviour was "ignore and carry on": unknown options, unique-prefix abbreviations and stray
//! positional arguments are errors. An invalid value is reported *here*, on the user's terminal,
//! instead of being discovered later in a daemon that nobody is watching.

use std::fmt;

use crate::error::{ProtocolError, ValidationError};
use crate::name::{InstanceName, ProviderName};
use crate::request::{Flags, ProviderRef, Request};
use crate::text::{parse_idle_timeout, UserAgent};
use crate::url::ProviderUrl;

/// A parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    /// `--instance NAME`: selects socket, state file and data directories. Not part of the request.
    pub instance: Option<InstanceName>,
    /// What to ask the daemon.
    pub request: Request,
}

/// Result of parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Run this.
    Run(Invocation),
    /// `-h` / `--help`
    Help,
    /// `-V` / `--version`
    Version,
}

/// A command line that cannot be run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliError {
    /// An option this program does not have.
    UnknownOption {
        /// What was typed.
        given: String,
        /// The only option it is a prefix of, if there is exactly one.
        suggestion: Option<&'static str>,
    },
    /// Option needs a value and none (or something that looks like an option) followed.
    MissingValue(&'static str),
    /// A flag that takes no value was given one (`--toggle=1`).
    UnexpectedValue(&'static str),
    /// A bare word where only options are allowed.
    UnexpectedArgument(String),
    /// The value was refused by validation.
    Invalid {
        /// Option the value belongs to.
        option: &'static str,
        /// Why.
        reason: ValidationError,
    },
    /// `--provider` value is neither a valid name nor a valid URL.
    BadProvider {
        /// Why it is not a name.
        as_name: ValidationError,
        /// Why it is not a URL.
        as_url: ValidationError,
    },
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOption {
                given,
                suggestion: Some(s),
            } => {
                write!(f, "unknown option '{given}' (abbreviations are not accepted; did you mean '{s}'?)")
            }
            Self::UnknownOption {
                given,
                suggestion: None,
            } => write!(f, "unknown option '{given}'"),
            Self::MissingValue(o) if *o == "--add-provider" => {
                f.write_str("--add-provider needs two arguments: NAME URL")
            }
            Self::MissingValue(o) => write!(f, "{o} needs a value"),
            Self::UnexpectedValue(o) => write!(f, "{o} does not take a value"),
            Self::UnexpectedArgument(a) => write!(f, "unexpected argument '{a}'"),
            Self::Invalid { option, reason } => write!(f, "{option}: {reason}"),
            Self::BadProvider { as_name, as_url } => {
                write!(
                    f,
                    "--provider: not a provider name ({as_name}) and not a URL ({as_url})"
                )
            }
        }
    }
}

impl std::error::Error for CliError {}

const FLAG_OPTIONS: [(&str, Flags); 8] = [
    ("--toggle", Flags::TOGGLE),
    ("--show", Flags::SHOW),
    ("--hide", Flags::HIDE),
    ("--new-chat", Flags::NEW_CHAT),
    ("--soft-clean", Flags::SOFT_CLEAN),
    ("--hard-clean", Flags::HARD_CLEAN),
    ("--background", Flags::BACKGROUND),
    ("--quit", Flags::QUIT),
];

const VALUE_OPTIONS: [&str; 6] = [
    "--idle-timeout",
    "--user-agent",
    "--provider",
    "--add-provider",
    "--remove-provider",
    "--instance",
];

/// Usage text for `--help`.
pub const USAGE: &str = "\
Usage: llm-wrapper [OPTIONS]

With no options: toggle the window of a running daemon, or start one.

Window:
  --toggle                  show the window if hidden, hide it if shown
  --show                    show and focus the window
  --hide                    hide the window
  --background              start (or keep) the daemon with the window hidden
  --new-chat                open a clean chat with the active provider
  --quit                    shut the daemon down

Providers:
  --provider NAME|URL       switch to a provider from the list, or to any http(s) URL
  --add-provider NAME URL   add or replace a provider
  --remove-provider NAME    remove a provider

Memory:
  --soft-clean              release the in-memory cache
  --hard-clean              purge disk and DOM caches (keeps login cookies)
  --idle-timeout MINUTES    minutes hidden before the deep memory trim (default 15)

Other:
  --user-agent STRING       override the User-Agent
  --instance NAME           address a separate daemon (own socket, window class and data)
  -h, --help                print this help
  -V, --version             print the version

Names: letters, digits, '_' and '-', at most 32 characters.
URLs: https://..., or http:// for localhost / loopback addresses only.
";

/// Would argparse have taken `arg` for an option rather than a value? (`-` alone, negative numbers
/// and anything containing a space are values.)
fn looks_like_option(arg: &str) -> bool {
    arg.starts_with('-') && arg.len() > 1 && !is_negative_number(arg) && !arg.contains(' ')
}

/// argparse's `^-\d+$|^-\d*\.\d+$`, restricted to ASCII digits.
fn is_negative_number(arg: &str) -> bool {
    let Some(rest) = arg.strip_prefix('-') else {
        return false;
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    match rest.split_once('.') {
        None => !rest.is_empty() && digits(rest),
        Some((int, frac)) => digits(int) && !frac.is_empty() && digits(frac),
    }
}

struct Cursor<'a> {
    args: &'a [String],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// Next argument if it can be a value.
    fn value(&mut self, option: &'static str) -> Result<&'a str, CliError> {
        let next = self
            .args
            .get(self.pos)
            .ok_or(CliError::MissingValue(option))?;
        if looks_like_option(next) {
            return Err(CliError::MissingValue(option));
        }
        self.pos += 1;
        Ok(next)
    }
}

fn invalid<T>(option: &'static str, r: Result<T, ValidationError>) -> Result<T, CliError> {
    r.map_err(|reason| CliError::Invalid { option, reason })
}

fn unknown(given: &str) -> CliError {
    let name = given.split_once('=').map_or(given, |(n, _)| n);
    let mut hits = FLAG_OPTIONS
        .iter()
        .map(|(n, _)| *n)
        .chain(VALUE_OPTIONS)
        .filter(|n| name.len() > 2 && n.starts_with(name));
    let suggestion = match (hits.next(), hits.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    };
    CliError::UnknownOption {
        given: given.to_owned(),
        suggestion,
    }
}

/// Parse the arguments that follow the program name.
pub fn parse(args: &[String]) -> Result<Parsed, CliError> {
    let mut request = Request::default();
    let mut instance = None;
    let mut cur = Cursor { args, pos: 0 };

    while let Some(arg) = args.get(cur.pos) {
        cur.pos += 1;
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            _ => {}
        }
        let Some(long) = arg.strip_prefix("--").filter(|l| !l.is_empty()) else {
            return Err(if arg.len() > 1 && arg.starts_with('-') {
                unknown(arg)
            } else {
                CliError::UnexpectedArgument(arg.clone())
            });
        };
        let (name, inline) = match long.split_once('=') {
            Some((n, v)) => (&arg[..2 + n.len()], Some(v)),
            None => (arg.as_str(), None),
        };

        if let Some((flag_name, flag)) = FLAG_OPTIONS.iter().find(|(n, _)| *n == name) {
            if inline.is_some() {
                return Err(CliError::UnexpectedValue(flag_name));
            }
            request.flags.insert(*flag);
            continue;
        }
        let Some(&option) = VALUE_OPTIONS.iter().find(|n| **n == name) else {
            return Err(unknown(arg));
        };

        if option == "--add-provider" {
            if inline.is_some() {
                return Err(CliError::MissingValue(option));
            }
            let n = cur.value(option)?;
            let u = cur.value(option)?;
            request.add_provider = Some((
                invalid(option, ProviderName::parse(n))?,
                invalid(option, ProviderUrl::parse(u))?,
            ));
            continue;
        }

        let value = match inline {
            Some(v) => v,
            None => cur.value(option)?,
        };
        match option {
            "--idle-timeout" => {
                request.idle_timeout_minutes = Some(invalid(option, parse_idle_timeout(value))?);
            }
            "--user-agent" => request.user_agent = Some(invalid(option, UserAgent::parse(value))?),
            "--remove-provider" => {
                request.remove_provider = Some(invalid(option, ProviderName::parse(value))?);
            }
            "--instance" => instance = Some(invalid(option, InstanceName::parse(value))?),
            _ => {
                request.provider = Some(
                    match (ProviderName::parse(value), ProviderUrl::parse(value)) {
                        (Ok(n), _) => ProviderRef::Name(n),
                        (_, Ok(u)) => ProviderRef::Url(u),
                        (Err(as_name), Err(as_url)) => {
                            return Err(CliError::BadProvider { as_name, as_url })
                        }
                    },
                );
            }
        }
    }
    Ok(Parsed::Run(Invocation { instance, request }))
}

impl From<CliError> for ProtocolError {
    fn from(e: CliError) -> Self {
        match e {
            CliError::Invalid { reason, .. } => ProtocolError::Invalid(reason),
            _ => ProtocolError::Truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Invocation, CliError> {
        let v: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        match parse(&v)? {
            Parsed::Run(i) => Ok(i),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn empty_command_line_is_an_empty_request() {
        let i = run(&[]).unwrap();
        assert_eq!(i.request, Request::default());
        assert_eq!(i.instance, None);
    }

    #[test]
    fn flags_accumulate_and_repeat_harmlessly() {
        let i = run(&["--toggle", "--new-chat", "--toggle"]).unwrap();
        assert!(i.request.flags.contains(Flags::TOGGLE));
        assert!(i.request.flags.contains(Flags::NEW_CHAT));
        assert!(!i.request.flags.contains(Flags::SHOW));
    }

    #[test]
    fn values_by_space_or_equals_and_last_one_wins() {
        let i = run(&[
            "--user-agent=A=B",
            "--idle-timeout",
            "9",
            "--idle-timeout=11",
        ])
        .unwrap();
        assert_eq!(i.request.user_agent.unwrap().as_str(), "A=B");
        assert_eq!(i.request.idle_timeout_minutes, Some(11));
        let i = run(&["--provider", "a", "--provider", "https://x.example"]).unwrap();
        assert!(matches!(i.request.provider, Some(ProviderRef::Url(_))));
    }

    #[test]
    fn option_like_values_follow_argparse() {
        assert!(run(&["--user-agent", "-foo"]).is_err());
        assert!(run(&["--user-agent", "-foo bar"]).is_ok());
        assert!(run(&["--user-agent=-foo"]).is_ok());
        assert!(run(&["--provider", "-5"]).is_ok());
        assert!(run(&["--idle-timeout", "-5"]).is_ok());
        assert!(run(&["--idle-timeout", "--show"]).is_err());
        assert!(
            is_negative_number("-5") && is_negative_number("-.5") && is_negative_number("-5.25")
        );
        assert!(
            !is_negative_number("-") && !is_negative_number("-5.") && !is_negative_number("-a")
        );
    }

    #[test]
    fn strictness_where_legacy_was_silent() {
        assert_eq!(
            run(&["--toogle"]),
            Err(CliError::UnknownOption {
                given: "--toogle".into(),
                suggestion: None
            })
        );
        assert_eq!(
            run(&["--tog"]),
            Err(CliError::UnknownOption {
                given: "--tog".into(),
                suggestion: Some("--toggle")
            })
        );
        // `--h` is a prefix of several options: no guess.
        assert_eq!(
            run(&["--h"]),
            Err(CliError::UnknownOption {
                given: "--h".into(),
                suggestion: None
            })
        );
        assert_eq!(
            run(&["foo"]),
            Err(CliError::UnexpectedArgument("foo".into()))
        );
        assert_eq!(run(&[""]), Err(CliError::UnexpectedArgument(String::new())));
        assert_eq!(run(&["-"]), Err(CliError::UnexpectedArgument("-".into())));
        assert!(matches!(
            run(&["--"]),
            Err(CliError::UnexpectedArgument(_) | CliError::UnknownOption { .. })
        ));
        assert_eq!(
            run(&["--toggle=1"]),
            Err(CliError::UnexpectedValue("--toggle"))
        );
        assert!(matches!(run(&["-x"]), Err(CliError::UnknownOption { .. })));
    }

    #[test]
    fn add_provider_takes_exactly_two_values() {
        let i = run(&["--add-provider", "m", "https://chat.mistral.ai"]).unwrap();
        assert_eq!(i.request.add_provider.unwrap().0.as_str(), "m");
        assert_eq!(
            run(&["--add-provider", "m"]),
            Err(CliError::MissingValue("--add-provider"))
        );
        assert_eq!(
            run(&["--add-provider=m", "https://x.example"]),
            Err(CliError::MissingValue("--add-provider"))
        );
        assert_eq!(
            run(&["--add-provider", "m", "https://x.example", "extra"]),
            Err(CliError::UnexpectedArgument("extra".into()))
        );
    }

    #[test]
    fn provider_is_a_name_or_a_url() {
        assert!(matches!(
            run(&["--provider", "gemini"]).unwrap().request.provider,
            Some(ProviderRef::Name(_))
        ));
        assert!(matches!(
            run(&["--provider", "http://localhost:3000"])
                .unwrap()
                .request
                .provider,
            Some(ProviderRef::Url(_))
        ));
        assert!(matches!(
            run(&["--provider", "not a url"]),
            Err(CliError::BadProvider { .. })
        ));
        assert!(matches!(
            run(&["--provider", "http://example.com"]),
            Err(CliError::BadProvider { .. })
        ));
    }

    #[test]
    fn help_and_version() {
        let v = |a: &[&str]| parse(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
        assert_eq!(v(&["--help"]), Ok(Parsed::Help));
        assert_eq!(v(&["--toggle", "-h"]), Ok(Parsed::Help));
        assert_eq!(v(&["-V"]), Ok(Parsed::Version));
        assert!(USAGE.contains("--add-provider NAME URL"));
    }

    #[test]
    fn instance_is_validated_but_not_part_of_the_request() {
        let i = run(&["--instance=work", "--show"]).unwrap();
        assert_eq!(i.instance.unwrap().as_str(), "work");
        assert!(run(&["--instance", "bad name"]).is_err());
        assert!(run(&["--instance", ""]).is_err());
    }

    #[test]
    fn every_error_has_a_readable_message() {
        for args in [
            &["--tog"][..],
            &["--idle-timeout"],
            &["--provider", "x y"],
            &["foo"],
            &["--toggle=1"],
        ] {
            let msg = run(args).unwrap_err().to_string();
            assert!(!msg.is_empty() && !msg.contains("Err("), "{msg}");
        }
    }
}
