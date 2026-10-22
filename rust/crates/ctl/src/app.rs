//! The `llm-wrapper` command: parse, verify the runtime directory, then either hand the request to
//! the running daemon or start one.
//!
//! Exit codes: `0` done, `1` the daemon refused or failed, `2` usage error, `3` the daemon cannot be
//! reached or started, `4` a security check failed.

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;

use llm_wrapper_protocol::{parse_args, sanitize_for_terminal, Flags, Parsed, Status, USAGE};

use crate::client::{self, ClientError};
use crate::daemon::{self, DaemonError};
use crate::runtime::{self, RuntimeError, SocketState};

/// Done.
pub const EXIT_OK: u8 = 0;
/// The daemon refused or failed.
pub const EXIT_FAILED: u8 = 1;
/// Bad command line.
pub const EXIT_USAGE: u8 = 2;
/// Daemon unreachable or not startable.
pub const EXIT_UNREACHABLE: u8 = 3;
/// A security check refused to continue.
pub const EXIT_INSECURE: u8 = 4;

fn report(err: &mut dyn Write, msg: &dyn std::fmt::Display) {
    // A failed write to stderr has nowhere to be reported.
    let _ = writeln!(
        err,
        "llm-wrapper: {}",
        sanitize_for_terminal(&msg.to_string())
    );
}

fn runtime_failure(err: &mut dyn Write, e: &RuntimeError) -> u8 {
    report(err, e);
    EXIT_INSECURE
}

/// Run the command. `exe` is the path of this executable (needed only to start the daemon).
pub fn run(
    args: &[String],
    xdg_runtime_dir: Option<&OsStr>,
    exe: Option<&Path>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let invocation = match parse_args(args) {
        Ok(Parsed::Help) => {
            let _ = out.write_all(USAGE.as_bytes());
            return EXIT_OK;
        }
        Ok(Parsed::Version) => {
            let _ = writeln!(out, "llm-wrapper {}", env!("CARGO_PKG_VERSION"));
            return EXIT_OK;
        }
        Ok(Parsed::Run(i)) => i,
        Err(e) => {
            report(err, &e);
            let _ = writeln!(err, "Try 'llm-wrapper --help' for the list of options.");
            return EXIT_USAGE;
        }
    };

    let uid = match runtime::current_uid() {
        Ok(u) => u,
        Err(e) => return runtime_failure(err, &e),
    };
    let dir = runtime::runtime_dir(xdg_runtime_dir, uid);
    if let Err(e) = runtime::verify_runtime_dir(&dir, uid) {
        return runtime_failure(err, &e);
    }
    let socket = match runtime::socket_path(&dir, invocation.instance.as_ref()) {
        Ok(p) => p,
        Err(e) => return runtime_failure(err, &e),
    };
    let state = match runtime::verify_socket(&socket, uid) {
        Ok(s) => s,
        Err(e) => return runtime_failure(err, &e),
    };

    let mut request = invocation.request;
    // The legacy launcher turned a bare `llm-wrapper` into a toggle when a daemon was running.
    if args.is_empty() {
        request.flags.insert(Flags::TOGGLE);
    }

    if state == SocketState::Present {
        match client::connect(&socket, client::TIMEOUT)
            .and_then(|stream| client::exchange(&stream, &request, client::TIMEOUT))
        {
            Ok(response) => return finish(&response, err),
            Err(ClientError::NotRunning) => {} // stale socket: fall through and start a daemon
            Err(ClientError::Protocol(e)) => {
                report(err, &format!("unexpected reply from the daemon: {e}"));
                let _ = writeln!(
                    err,
                    "llm-wrapper: another version may be running; stop it (llm-wrapper --quit) and try again."
                );
                return EXIT_UNREACHABLE;
            }
            Err(e) => {
                report(err, &e);
                return EXIT_UNREACHABLE;
            }
        }
    }

    // No daemon. Quitting something that is not running is already done.
    if request.flags.contains(Flags::QUIT) {
        return EXIT_OK;
    }
    start_daemon(args, exe, uid, err)
}

fn start_daemon(args: &[String], exe: Option<&Path>, uid: u32, err: &mut dyn Write) -> u8 {
    let located = exe
        .ok_or(DaemonError::NoExePath)
        .and_then(|e| daemon::locate(e, uid));
    match located {
        Ok(path) => {
            let e = daemon::exec(&path, args);
            report(err, &e);
            EXIT_UNREACHABLE
        }
        Err(e @ DaemonError::Untrusted { .. }) => {
            report(err, &e);
            EXIT_INSECURE
        }
        Err(e) => {
            report(err, &e);
            EXIT_UNREACHABLE
        }
    }
}

fn finish(response: &llm_wrapper_protocol::Response, err: &mut dyn Write) -> u8 {
    if !response.message.is_empty() {
        report(err, &response.message);
    }
    match response.status {
        Status::Ok => EXIT_OK,
        Status::Rejected | Status::Busy | Status::Internal => EXIT_FAILED,
        Status::Unsupported => EXIT_UNREACHABLE,
    }
}
