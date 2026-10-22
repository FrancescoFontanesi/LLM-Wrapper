//! `llm-wrapperd` — the GTK4 + `WebKitGTK` window and its control socket.
//!
//! Normally started by the `llm-wrapper` command when no daemon runs; it takes the same command
//! line. If a daemon for the same instance already runs, the command is handed over to it.

mod app;
mod core;
mod instance;
mod ipc;
mod log;
mod paths;
mod providers;
mod recovery;
mod state;
mod ui;

use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk4::glib;
use llm_wrapper_platform::runtime;
use llm_wrapper_protocol::{
    parse_args, sanitize_for_terminal, Flags, Parsed, Request, Status, USAGE,
};

/// How long a second process waits for the first to start listening before giving up.
const FORWARD_PATIENCE: Duration = Duration::from_secs(10);

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_UNREACHABLE: u8 = 3;
const EXIT_INSECURE: u8 = 4;

fn fail(code: u8, message: &str) -> ExitCode {
    log::error(message);
    ExitCode::from(code)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn main() -> ExitCode {
    // The 30 fps cap WebKit applies when the system is in power-saver mode comes from the power
    // profile read over the *system* bus; without that bus (every other service here uses the session
    // bus) scrolling and streaming stay smooth. Done first: nothing else is running yet.
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", "unix:path=/nonexistent");

    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        let Ok(text) = arg.into_string() else {
            return fail(EXIT_USAGE, "arguments must be valid UTF-8");
        };
        args.push(text);
    }
    let invocation = match parse_args(&args) {
        Ok(Parsed::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            println!("llm-wrapperd {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(i)) => i,
        Err(e) => return fail(EXIT_USAGE, &e.to_string()),
    };
    let instance = invocation.instance;
    let request = invocation.request;

    let uid = match runtime::current_uid() {
        Ok(u) => u,
        Err(e) => return fail(EXIT_INSECURE, &e.to_string()),
    };
    let dirs = match paths::resolve(&paths::Env::from_process(), uid, instance.as_ref()) {
        Ok(d) => d,
        Err(e) => return fail(EXIT_FAILED, &e),
    };
    match paths::prepare(&dirs, uid) {
        Ok(notes) => notes.iter().for_each(|n| log::warn(n)),
        Err(e) => return fail(EXIT_INSECURE, &e),
    }
    let socket = match runtime::socket_path(&dirs.runtime, instance.as_ref()) {
        Ok(p) => p,
        Err(e) => return fail(EXIT_INSECURE, &e.to_string()),
    };

    let (_lock, _) = match instance::take_lock(&dirs.runtime, instance.as_ref(), uid) {
        Ok(Some(held)) => held,
        Ok(None) => return hand_over(&socket, uid, &request),
        Err(e) => return fail(EXIT_INSECURE, &e),
    };

    // ---- we are the daemon ---------------------------------------------------------------------
    let prgname = instance
        .as_ref()
        .map_or_else(|| "llm-wrapper".to_owned(), |i| format!("llm-wrapper-{i}"));
    glib::set_prgname(Some(&prgname)); // the window class compositor rules match on
    glib::set_application_name("LLM");
    if let Err(e) = gtk4::init() {
        return fail(EXIT_FAILED, &format!("cannot initialise GTK: {e}"));
    }

    let providers_file = dirs.config.join("providers.json");
    let loaded = providers::load(&providers_file, uid, now_unix());
    loaded.notes.iter().for_each(|n| log::warn(n));

    let ui = match ui::Ui::new(&dirs.data, &dirs.cache) {
        Ok(ui) => ui,
        Err(e) => return fail(EXIT_FAILED, &e),
    };
    let state_path = dirs.runtime.join(match &instance {
        Some(i) => format!("llm-wrapper-{i}.json"),
        None => "llm-wrapper.json".to_owned(),
    });
    let main_loop = glib::MainLoop::new(None, false);
    let app = app::App::new(
        ui,
        loaded.providers,
        providers_file,
        state::StateFile::new(state_path),
        instance.as_ref().map(ToString::to_string),
        main_loop.clone(),
    );

    let listener = match instance::bind_socket(&socket, uid) {
        Ok(l) => l,
        Err(e) => return fail(EXIT_FAILED, &e),
    };
    let server = match ipc::start(listener, command_handler(&app), quit_hook(&app)) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&socket);
            return fail(EXIT_FAILED, &e);
        }
    };
    quit_on_signals(&app);

    // The command that started us is the first command the daemon carries out. A bare start
    // (`llm-wrapper` with no arguments) is a toggle, as the client would have sent.
    let mut first = request;
    if args.is_empty() {
        first.flags.insert(Flags::TOGGLE);
    }
    let reply = app.execute(&first);
    let mut code = ExitCode::SUCCESS;
    if reply.response.status != Status::Ok {
        // a command that was refused must not leave a daemon behind
        log::error(&reply.response.message);
        code = ExitCode::from(EXIT_FAILED);
    } else if !reply.then_quit {
        main_loop.run();
    }

    // ---- shutdown ------------------------------------------------------------------------------
    server.stop();
    app.remove_state();
    let _ = std::fs::remove_file(&socket);
    code
}

/// Runs every command that arrives on the socket. Holds the app weakly: once shutdown starts the
/// handler must not keep it alive.
fn command_handler(app: &Rc<app::App>) -> ipc::Handler {
    let weak = Rc::downgrade(app);
    Rc::new(move |req: Request| match weak.upgrade() {
        Some(app) => app.execute(&req),
        None => ipc::Reply {
            response: llm_wrapper_protocol::Response::with_message(
                Status::Internal,
                "shutting down",
            ),
            then_quit: false,
        },
    })
}

/// What the socket calls to stop the daemon after a `--quit` has been answered.
fn quit_hook(app: &Rc<app::App>) -> Rc<dyn Fn()> {
    let weak = Rc::downgrade(app);
    Rc::new(move || {
        if let Some(app) = weak.upgrade() {
            app.quit();
        }
    })
}

/// SIGHUP, SIGINT and SIGTERM stop the daemon the same way `--quit` does.
fn quit_on_signals(app: &Rc<app::App>) {
    for signum in [1, 2, 15] {
        let weak = Rc::downgrade(app);
        glib::unix_signal_add_local(signum, move || {
            if let Some(app) = weak.upgrade() {
                app.quit();
            }
            glib::ControlFlow::Break
        });
    }
}

/// Another daemon owns this instance: give it the command and report what it says.
fn hand_over(socket: &std::path::Path, uid: u32, request: &Request) -> ExitCode {
    let mut request = request.clone();
    // a start with no options at all means "toggle"
    if request == Request::default() {
        request.flags.insert(Flags::TOGGLE);
    }
    match instance::forward(socket, uid, &request, FORWARD_PATIENCE) {
        Ok(response) => {
            if !response.message.is_empty() {
                log::warn(&sanitize_for_terminal(&response.message));
            }
            match response.status {
                Status::Ok => ExitCode::SUCCESS,
                Status::Rejected | Status::Busy | Status::Internal => ExitCode::from(EXIT_FAILED),
                Status::Unsupported => ExitCode::from(EXIT_UNREACHABLE),
            }
        }
        Err(e) => fail(EXIT_UNREACHABLE, &e),
    }
}
