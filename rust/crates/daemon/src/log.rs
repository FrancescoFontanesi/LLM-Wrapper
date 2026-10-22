//! Logging to stderr. Text that came from outside (file contents, peers, web pages) is sanitised
//! before it reaches a terminal or a journal.

use std::io::Write;

use llm_wrapper_protocol::sanitize_for_terminal;

fn emit(level: &str, msg: &str) {
    // a failed write to stderr has nowhere to be reported (and `eprintln!` would panic)
    let _ = writeln!(
        std::io::stderr(),
        "llm-wrapperd: {level}{}",
        sanitize_for_terminal(msg)
    );
}

pub fn info(msg: &str) {
    emit("", msg);
}

pub fn warn(msg: &str) {
    emit("warning: ", msg);
}

pub fn error(msg: &str) {
    emit("error: ", msg);
}
