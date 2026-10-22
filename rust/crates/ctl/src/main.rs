use std::io::{self, Write};
use std::process::ExitCode;

use llm_wrapper_ctl::app;

fn main() -> ExitCode {
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        let Ok(text) = arg.into_string() else {
            // No `eprintln!`: it panics (and with panic=abort, aborts) if stderr is closed.
            let _ = writeln!(io::stderr(), "llm-wrapper: arguments must be valid UTF-8");
            return ExitCode::from(app::EXIT_USAGE);
        };
        args.push(text);
    }
    let xdg = std::env::var_os("XDG_RUNTIME_DIR");
    let exe = std::env::current_exe().ok();
    let code = app::run(
        &args,
        xdg.as_deref(),
        exe.as_deref(),
        &mut io::stdout(),
        &mut io::stderr(),
    );
    ExitCode::from(code)
}
