# Security model

What the Rust code defends against, what it deliberately does not, and the test that proves each
claim. Written for Phase 1 (protocol + client); sections marked **Phase 2+** are requirements the
daemon must meet, already encoded as tests that currently skip because the daemon does not exist.

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
inner attribute); production crates have **no third-party dependencies** (`cargo tree -e normal`
shows only `llm-wrapper-protocol`; `serde_json` and `url` appear only in tests); release builds use
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

## Requirements for the daemon (Phase 2+), already encoded as tests

| Requirement | Test (skips until the daemon exists) |
|---|---|
| Login data, cache, config, socket, state file: directories `0700`, files `0600`; verified at start, not just created (no symlinks, owned by us). | `d07` |
| The socket is created `0600` in a verified `0700` directory and each connection's peer uid is checked (`SO_PEERCRED`). | `d07` + new test |
| No head-of-line blocking: one idle connection cannot delay others; every connection has a deadline. | `d10` |
| Reply after acting; refusals reach the caller. | `d01`, `d02` |
| A corrupt provider list is preserved, not overwritten. | `d08` |
| Providers are `https` (or loopback `http`); navigation to `file:`, `javascript:`, `data:` as a top-level target is blocked; external links go to the default browser. | `d03`, `d02`; navigation policy tests in Phase 3 |
| Web content runs in the WebKit sandbox. **Measured:** WebKitGTK 4.1 (the legacy app) starts its web process with *no* sandbox; WebKitGTK 6.0 starts it inside `bwrap` + `xdg-dbus-proxy`. This is the concrete security reason for GTK4. | Phase 2 check |
| Permissions (microphone, camera, geolocation, notifications) denied by default; DevTools off by default. | Phase 3 |
| Single instance enforced by the socket and a lock, never by unlinking a live socket. | `p11`, `p12` |

## Known residual risks (Phase 1)

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
