# Security model

What the Rust code defends against, what it deliberately does not, and the test that proves each
claim. Written for Phases 1 (protocol + client) and 2 (daemon). Each control names the test that proves it;
the last section lists what is not defended or not yet done.

## Assets and boundaries

| Asset | Where | Why it matters |
|---|---|---|
| Logged-in sessions of AI chats (cookies, local storage) | WebKit data directory | Account takeover; chat history. |
| The daemon's control socket | `$XDG_RUNTIME_DIR` | Whoever can write to it can make the wrapper navigate, add providers, change the User-Agent, quit. |
| The `llm-wrapperd` binary | install directory | Run with the user's full privileges. |
| What the user reads on the terminal | client output | Escape sequences can rewrite it. |

**Attacker we defend against:** another local user, or a sandboxed/unprivileged program that can
touch world-accessible locations, planting files or sending bytes to places we trust; a buggy or
compromised peer on the socket; hostile web content reaching the daemon through provider URLs.

**Not defended (same trust domain):** another process running as *the same user*. It can already read
the cookies, ptrace the daemon and replace the binary; no wrapper can change that.

## Controls in the client (Phase 1) and their proof

| # | Control | Test |
|---|---|---|
| 1 | The runtime directory must be absolute, a real directory (not a symlink), owned by us, with no group/other bits (`0700`). Refused otherwise — never repaired, never bypassed. Exit 4. | `runtime::tests::refuses_unsafe_directories`, `hardening::runtime_dir_*`, e2e `c04` |
| 2 | The socket must be a socket (not a symlink or file), owned by us, `0600`. | `hardening::socket_open_to_others_is_refused`, `…symlink_or_regular_file…`, `…socket_owned_by_someone_else…` |
| 3 | Socket path must fit `sun_path` instead of being silently truncated onto another path. | `over_long_socket_path_is_refused` |
| 4 | All checks use `lstat`; no link is ever followed. | same tests as 1–2 |
| 5 | Our own uid comes from `/proc/self` without `unsafe`; if it cannot be read the client **fails closed**. | `runtime::current_uid` (used by every test) |
| 6 | Versioned socket name: a v1 client never talks to the legacy daemon (which executes whatever it receives as arguments). | `the_legacy_socket_is_never_spoken_to` |
| 7 | Frames are bounded (8 KiB request, 1 KiB response) and the bound is checked before reading the body; a hostile peer cannot make us allocate or read more. | `oversized_declarations_do_not_allocate_or_read_unboundedly`, `a_flooding_daemon_is_not_read_beyond_the_header` |
| 8 | Deadlines: connect 0.8 s, and the *whole* reply 0.8 s even if the peer sends one byte at a time. A hung daemon cannot hang a hotkey. | `a_silent_daemon_cannot_hang_the_hotkey`, `a_trickling_daemon_is_bounded_by_the_total_deadline_not_per_byte` |
| 9 | Canonical encoding: one byte string per request, no skipped fields. Nothing to smuggle past a filter. | `arbitrary_bytes_never_panic_and_stay_canonical`, `mutated_frames_never_panic_and_stay_canonical`, `request::tests::decoding_is_canonical` |
| 10 | The receiver re-validates every field; the client's checks are never the only line. | `request::tests::decode_revalidates_every_value` |
| 11 | URL validation refuses what it does not fully understand: credentials, backslashes, non-ASCII, `%` in hosts, `http` to non-loopback, and hosts a WHATWG parser reads as numbers (`2130706433`, `0x7f.1`, `127.1`). Differentially tested against the `url` crate on 60 000 random URLs: nothing we accept is read differently. | `url::tests::*`, `known_parser_differential_tricks_are_refused`, `random_urls_never_disagree_with_the_reference_parser` |
| 12 | User-Agent printable ASCII ≤ 512 (no header injection); names `[A-Za-z0-9_-]{1,32}`; idle timeout bounded. | `text::tests::*`, `name::tests::*` |
| 13 | Daemon text is sanitised before it reaches the terminal (control characters, bidi overrides → `?`). | `daemon_text_cannot_rewrite_the_terminal`, `text::tests::terminal_sanitising` |
| 14 | The daemon to start is `llm-wrapperd` **next to the client**, never from `PATH` or an environment variable. | `the_daemon_is_never_taken_from_path` |
| 15 | …and only if the file is a regular file (no symlink), owned by us or root, not group/other-writable, executable; its directory not group/other-writable; every ancestor owned by us/root and not world-writable unless sticky. | `untrustworthy_daemons_are_refused_and_not_executed`, `a_daemon_owned_by_someone_else_is_refused`, `an_install_directory_writable_by_others_is_refused`, `a_world_writable_ancestor_is_refused_unless_it_is_sticky` |
| 16 | A bad command line is rejected before any socket or process is touched. | `usage_errors_exit_2_without_touching_the_daemon`, `an_invalid_command_line_never_starts_a_daemon` |
| 17 | Non-UTF-8 and oversized arguments are usage errors. | `non_utf8_and_oversized_arguments_are_usage_errors` |

Supply chain and language: `unsafe` is `forbid`den workspace-wide (cannot be re-allowed by an
inner attribute); `protocol`, `platform` and `ctl` — everything the *client* runs — have **no
third-party dependencies** (`cargo tree -e normal -p llm-wrapper-ctl`; `serde_json` and `url`
appear only in tests). The daemon adds exactly three: `gtk4`, `webkit6` (the gtk-rs bindings, whose
own `unsafe` is the price of talking to a C toolkit) and `serde_json`. Release builds use
`panic = "abort"`, `--locked`, and clippy `all` + `pedantic` as errors. The client builds with Rust
1.80 and is released as a static-pie binary (no loader, no shared libraries).

### How the tests themselves were checked

Passing tests prove little unless they can fail. The following were broken on purpose and each
break was caught by at least one test: 16 removed client checks (mode, owner, symlink, socket type,
absolute path, path length, daemon-file writable/symlink/owner, ancestor walk, sticky exception,
install-dir mode, quit-without-daemon, bare-toggle, exit code 4, stale-socket handling); the total
reply deadline; terminal sanitising; the protocol-error advice; unknown-option rejection; an
off-by-one in the idle timeout; the numeric-host rule and the `http`-loopback rule; and 12 faults
injected into the legacy app against the parity suite. Three of my first attempts were *not*
caught — the mutation had not been applied, was incomplete, or was exercised by the wrong filter —
which is why the results above were re-run with substitutions that fail loudly when they do not apply.

### How the daemon's tests were checked (Phase 2)

Same method: break the code on purpose, expect a test to fail, and treat a survivor as a finding.
32 faults in the platform and daemon logic (each plan rule, each ownership/mode/symlink check, atomic
replacement, lock, deadlines, the in-flight cap, answer-after-action) were run against the unit
tests: 30 were caught at once.

* **One survivor was a real bug, in production code.** The peer-credential check used
  `gio::Credentials::is_same_user`. For a *different* user GLib returns FALSE **without setting an
  error**, and the gio-rs binding then returns `Ok(())` in a release build (the check passes for
  everybody) and panics in a debug build. The check had no test; the new one (`ipc::tests::
  a_connection_from_another_user_is_refused`, a client run as uid 65534 against a socket opened to
  everybody) first *passed by accident* through the panic, which is how the mutation `|| true`
  survived. The daemon now compares `SO_PEERCRED` uids itself (`is_us`), fails closed when the
  credentials cannot be read, and the test also fails on any panic. All three mutations of that
  function (`|| true`, unreadable credentials accepted, back to `is_same_user`) are caught.
* The other survivor is an equivalent mutant: re-tightening a directory right after creating it
  with mode `0700` can never change anything, since a umask only removes bits. It stays as a guard
  against a directory that appears concurrently, next to the check that refuses one that is not ours.
* An end-to-end test (`s10`) found a second real bug the unit tests could not: WebKit rejects the
  memory-pressure settings (a conservative threshold not below the strict one) with a `CRITICAL` line
  on stderr and carries on — the conservative threshold silently stayed at WebKit's default. Fixed
  (the strict threshold is set first); `s10` now fails on any toolkit warning.

## Controls in the daemon (Phase 2) and their proof

| # | Control | Test |
|---|---|---|
| 18 | Data, cache and config roots, their parents created by us, and the leaf directories are `0700`, ours, not links; checked at **every** start. A loose mode on our own directory is tightened and logged; a symlink or a foreign owner is refused (exit 4). Everything the daemon writes itself (socket, state, lock, provider list) is `0600`; WebKit's own files inside a `0700` root keep WebKit's mode and are protected by the root. | `private::tests::*`, `paths::tests::*`, e2e `d07`, `s04`, `s06` |
| 19 | Atomic replacement: a sibling `*.tmp` created exclusively (`O_EXCL`, `0600`, so a planted link is never followed), then `rename`. Readers never see half a file; no temporary stays. | `private::tests::*`, `state::tests::*`, e2e `p05`, `p13`, `s04` |
| 20 | The provider list is read only if it is a regular file of ours that nobody else can write, at most 64 KiB. Otherwise it is **moved aside** (`providers.json.untrusted-<time>` / `.corrupt-<time>` / `.dropped-<time>`), never followed, never silently overwritten, and the built-in list takes over. | `providers::tests::*`, e2e `d08`, `s05` |
| 21 | One daemon per instance: an exclusive `flock` that the kernel releases when the process dies (no stale locks); the lock decides who may remove a stale socket, and only a socket of ours is ever removed. A live daemon's socket is never touched. | `instance::tests::*`, e2e `p11`, `p12`, `s03`, `s09` |
| 22 | The socket is bound inside the verified `0700` runtime directory and chmod-ed `0600`. Each connection's peer uid is read with `SO_PEERCRED` (through GIO, no `unsafe`) and anything but our own uid is dropped without a word — **even if the file permissions were opened up**. | `ipc::tests::a_connection_from_another_user_is_refused` (needs root: it runs a client as uid 65534; skipped otherwise), e2e `s04` |
| 23 | Every connection is its own task with a one-second deadline for the request and another for the answer; a client that connects and says nothing, trickles bytes, or stops half way costs a timer and is cut off. At most 32 at once; the rest are told *busy*. Neither affects anybody else. | `ipc::tests::*` (idle, trickling, flood), e2e `d10`, `s01`, `s02` |
| 24 | Garbage is not answered (bad magic, wrong frame kind); an unknown protocol version, an oversized or malformed request is answered with a reason. Frames are bounded before the body is read and every field is validated again by the daemon. | `ipc::tests::garbage_*`, `…clear_answer`, e2e `s01` |
| 25 | A command is answered after it has been carried out, so `OK` is true and a refusal reaches the caller. A refused first command leaves no daemon. | `ipc::tests::*`, e2e `d01`, `d02`, `s07` |
| 26 | The daemon holds nothing on to: file descriptors do not grow after a burst of 200 idle connections; no GLib/GTK/WebKit `CRITICAL` or `WARNING` appears in a normal session (this found a real misuse of the WebKit memory-pressure API during Phase 2). | e2e `s02`, `s10` |
| 27 | `SIGTERM`/`SIGINT`/`SIGHUP` remove socket and state; a killed daemon leaves nothing that blocks the next. | e2e `s08`, `s09` |
| 28 | Web content runs in the WebKit sandbox. **Measured:** WebKitGTK 4.1 (the legacy app) starts its web process with *no* sandbox; WebKitGTK 6.0 starts it inside `bwrap` + `xdg-dbus-proxy`. This is the concrete security reason for GTK4. `file:` navigation is refused. | observed in the process tree (`d09` finds `WebKitWebProcess` under `bwrap`); `app::forbid_file_navigation` |
| 29 | Camera and microphone are off (`enable_media_stream = false`), so no page can ask for them. DevTools are enabled **only** for the F12 key that the README promises. | spec V11 |

### Controls in the daemon for web content (Phase 3)

| # | Control | Test |
|---|---|---|
| 30 | **Only the web is navigated to.** `http(s)`, `about:`, `blob:` and `data:` are the only schemes a navigation may have, whoever starts it (a click, a redirect, a frame, a script); `file:`, `ftp:`, `mailto:`, `javascript:` and application schemes are refused and nothing is launched for them. An address that cannot be read the same way twice (backslashes, `%` or non-ASCII in the host, spaces, no host, over 8 KiB) is refused rather than guessed. | `nav::tests::*` (13, including the fixture below), e2e `k05` |
| 31 | **Links stay or leave by one rule**, taken from the legacy app and checked against it: the fixture is generated by running the legacy `is_internal`/`base_domain` lifted from `app.py` with `ast` (`gen_legacy_nav.py`); 58 cases, each either identical or a listed deviation (V16): an IP address is only itself, a public suffix is not one site, plain `http` and `user:password@` never stay in the window. | `nav::tests::agrees_with_the_legacy_app_except_where_deliberately_different`, e2e `k03`, `k04` (also on legacy) |
| 32 | **The browser is launched only for `http(s)`**, only for a click (a script's `window.open` gets nothing), at most twice a second, and the scheme is checked again at the launch site, so a mistake in the policy cannot start another program. The launch goes through GIO's default handler, not a program found in `PATH`. | e2e `s16`, `k03`, `k04`; unit tests for the rule |
| 33 | **Pop-ups**: a window for a trusted address opens in a window of its own that shares the session (sign-in flows need that); at most four; a refused window gets a view that is never shown, never loads anything and is closed at once. | e2e `s14`, `s15` |
| 34 | **Permissions**: every request (notifications, location, clipboard, devices, pointer lock, storage access) is denied explicitly and logged once per kind; camera and microphone do not exist (`enable_media_stream` off). | e2e `s13`, `k06` |
| 35 | **Files**: a download is saved only where the user says in a dialog, an upload receives only what the user picks; one dialog at a time, further requests are refused; only local files; the offered name is the last path component without control characters. | `transfers::tests::*`, e2e `s17`, `k07`, `k08`, `k09` |
| 36 | **Notifications** go straight to the notification service on the session bus, with control characters removed, markup escaped and a length bound; no `notify-send` from `PATH`. | `notify::tests::*`, e2e `k07` (read off the bus with `dbus-monitor`) |
| 37 | **The log never holds an address**, only its scheme: a refused sign-in redirect can carry a code or a token. | `policy::tests::the_log_gets_the_scheme_and_nothing_else` |

The gio-rs bindings turn `FALSE`-without-an-error into `Ok(())` in release builds (the pitfall that
broke the peer check, see above). The only other binding of that shape the daemon calls is
`launch_default_for_uri`, whose C function does set an error; it is used through its asynchronous
form and a failure is only logged.

## Known residual risks

* **Time-of-check / time-of-use.** The directory, socket and daemon are checked, then used. Closing
  this completely needs `openat`-style handles that Rust's standard library does not offer, so
  doing it would mean `unsafe` code or a dependency. It is mitigated, not eliminated: an attacker would have to modify a `0700`, user-owned
  directory, which is the "same user" case excluded above.
* **No peer-credential check on the client side.** `UnixStream::peer_cred` is unstable in std. The
  directory and socket ownership checks stand in for it; the daemon will check with `SO_PEERCRED`.
* **The legacy app stays insecure** until replaced (defects D03, D07, D10 above; the web process is
  not sandboxed). Running both daemons at once is prevented by their different socket names, not by
  design; do not.
* **A static glibc binary has to be rebuilt to pick up a glibc fix.** The client does no name
  resolution and speaks only to a local socket, which keeps the exposed surface small.
* **A same-uid process** can do anything, as above.
* **The compatibility script lies to the sites** (webdriver false, a Chrome brand list, a plugin
  list, a GPU name). It protects nothing; it is there because the bot checks of the chat sites
  need it, and it runs in every frame of every page, as in the legacy app. It cannot reach the app:
  it is page JavaScript.
* **Pop-ups share the login session.** A sign-in window must, so a trusted domain that opens a
  window can use the session of the provider it was opened from. The trusted list is the legacy
  one (`INTERNAL_DOMAINS` in `nav.rs`) plus the provider's own registrable domain.
* **`javascript_can_access_clipboard` stays on** (image paste with Ctrl+V needs it), as in the
  legacy app: a page can use the clipboard through the DOM commands, not the asynchronous API
  (which is denied, `k06`).
* **The Chrome version in the User-Agent will age** (V15); a site that stops accepting it is a
  one-constant change, but nothing tells you before it happens.
* **The portal file chooser is untested** (the suite has none); the dialog itself is GTK's.

