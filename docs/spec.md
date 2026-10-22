# llm-wrapper — behavioural specification

This document is the human-readable side of an **executable specification**. The legacy Python
app (`app.py` + the `llm-wrapper` bash launcher) is the reference for what "the same" means.

**"The same" means the same functionality and the same way of using it** — the commands and
options, the key bindings, the state file other tools read, the window class that compositor rules
match, the behaviour a user relies on. It does *not* mean the same internals or the same accidents:
where the legacy app does something that nobody could want (a defect, a side effect), the Rust
implementation is free to do the sensible thing, and the difference is written down here.

The black-box suite in `rust/crates/e2e` pins the behaviour down and runs unchanged against the
Rust implementation as it lands:

```bash
rust/scripts/e2e.sh                                  # the legacy app — what the spec is derived from
rust/scripts/e2e.sh --impl rust --rust-bin DIR       # the Rust client / daemon
rust/scripts/e2e.sh --bench baseline.json            # measurements (see "Baseline")
```

Nothing below is guessed from reading the code alone: every statement has a test, a fixture
generated from the real `app.py`, or is listed under [Not covered](#not-covered-by-automated-tests).

Test families (all under `rust/crates/e2e/tests/`):

| Prefix | File | Meaning |
|---|---|---|
| `p` | `parity.rs` | Must behave identically in legacy and Rust. |
| `d` | `defects.rs` | Legacy **fails** these (expected failure, enforced); Rust must pass. |
| `c` | `contract.rs` | Client-only contract; runs against the Rust client already (Phase 1). |
| `k` | `keyboard.rs` | Real key/mouse input on X11. Own binary: see the note in the file. |

A legacy test that unexpectedly *passes* fails the suite, so this document cannot drift.

---

## 1. Command line

`llm-wrapper [OPTIONS]` — all options are optional and combine.

| Option | Effect |
|---|---|
| *(none)* | Toggle the window of a running daemon; start one (visible) if none runs. |
| `--toggle` / `--show` / `--hide` | Visibility. A fresh daemon has no visible window, so `--toggle` shows it. |
| `--background` | Start (or keep) the daemon with the window hidden. |
| `--new-chat` | Load the active provider's root URL again **and show the window**. |
| `--soft-clean` / `--hard-clean` | Release the memory cache / purge disk and DOM caches (cookies kept). Visibility unchanged. |
| `--idle-timeout MIN` | Minutes hidden before the deep memory trim (default 15). Visibility unchanged. |
| `--provider NAME\|URL` | Switch provider (an ad-hoc URL is its own name) **and show the window**, as the README says. |
| `--add-provider NAME URL` / `--remove-provider NAME` | Edit the persisted provider list. Visibility unchanged. |
| `--user-agent STRING` | Override the User-Agent for subsequent loads. Visibility unchanged. |
| `--instance NAME` | Address a separate daemon: own socket, state file, window class, data. |
| `--quit` | Shut the daemon down. |
| `-h`, `--help`, `-V`, `--version` | **New** (the legacy app silently ignored them). |

Names are `[A-Za-z0-9_-]{1,32}`. Built-in providers (until a list is saved):
`chatgpt` → https://chatgpt.com (default), `gemini` → https://gemini.google.com,
`claude` → https://claude.ai.

### Parsing rules

`--opt value` and `--opt=value`; `--add-provider` takes exactly two following arguments; the last
occurrence of a repeated option wins; a value that starts with `-` is accepted only if it is a
negative number or contains a space (this is `argparse`'s rule, kept). Pinned by
`protocol/tests/legacy_cli.rs` against **98 cases produced by the legacy parser itself**
(`protocol/tests/fixtures/gen_legacy_cli.py` lifts the real `argparse` definition out of `app.py`
with `ast`; identical on Python 3.12 and 3.13).

### Exit codes (Rust client)

| Code | Meaning |
|---|---|
| 0 | Done. |
| 1 | The daemon refused the request or failed (message on stderr). |
| 2 | Bad command line (nothing was sent). |
| 3 | The daemon cannot be reached or started, or answered nonsense. |
| 4 | A security check refused to continue (see `security.md`). |

---

## 2. What the daemon does with a command (legacy order, kept)

`_execute_command_list` applies, in this order:

1. `--quit` → shut down **and ignore everything else on the line** *(test `p10`)*.
2. `--idle-timeout` (values below 1 become 1), `--user-agent`.
3. `--add-provider`, then `--remove-provider` (persisted immediately).
4. Create the window if needed; switch to `--provider` (or stay on the active one).
5. `--new-chat`, then `--soft-clean`, then `--hard-clean`.
6. Visibility, first match wins: `--toggle` > `--show` > `--hide` > `--background`(hide).
   With none of those, **`--new-chat` and `--provider` show the window and everything else leaves
   it as it is** (the legacy app shows it after *every* command; see D13). A daemon started by a
   command that does not show the window therefore starts hidden.

### Behaviours kept on purpose

* **Q1** `--toggle --show` hides a visible window (toggle wins); `--hide --show` shows (`p02`).
* **Q2** `--quit` wins over any other option (`p10`).
* **Q3** A bare `llm-wrapper` toggles when a daemon runs (bash launcher), whereas `python app.py`
  with no arguments shows (from reading the code; not tested). Rust follows the launcher.

Dropped as not useful: the legacy habit of showing the window after *any* command. It was kept
only where it serves the user — `--new-chat` is bound to a key to start a chat you are about to
read, and the README documents that `--provider` shows the window — and removed for cache
cleaning and settings, where a window popping up is only a distraction (`--hard-clean` is bound to
a key of its own).

---

## 3. Files, state and identity

| Item | Legacy | Rust (planned, Phase 2) |
|---|---|---|
| State file (public API, read by Quickshell) | `$XDG_RUNTIME_DIR/llm-wrapper[-NAME].json` | **same name, same schema** |
| Socket | `$XDG_RUNTIME_DIR/llm-wrapper[-NAME].sock` | `llm-wrapper[-NAME].v1.sock` (see V08) |
| Login data / cache | `./session`, `./cache` (`instances/NAME/…`) in the repository | `$XDG_DATA_HOME/llm-wrapper`, `$XDG_CACHE_HOME/llm-wrapper` |
| Provider list | `./providers.json` | `$XDG_CONFIG_HOME/llm-wrapper/providers.json` |
| Window class / Wayland `app_id` | `llm-wrapper[-NAME]` | **same** |

State file schema (exactly these keys, `p01`):

```json
{"instance": null, "active": "chatgpt", "url": "https://chatgpt.com",
 "visible": false, "providers": {"chatgpt": "https://chatgpt.com", "...": "..."}}
```

It is rewritten atomically (temp file + rename) on every change; a reader never sees half a
file (`p13`) and temporary files never stay (`p05`).

Window class: `WM_CLASS = ("llm-wrapper", "llm-wrapper")` on X11, `("llm-wrapper-NAME", …)` for an
instance (`p08`, `p09`). On Wayland the `app_id` is `llm-wrapper` — **checked by hand** on a headless
sway, not in the automated suite. With GTK4 a `GtkApplication` would turn the class into its
application-id and break users' Hyprland rules, so the Rust daemon must not use `GtkApplication`
(measured: plain `GtkWindow` + `prgname` keeps `llm-wrapper`).

---

## 4. Known defects of the legacy app (the `d` tests)

Each is an *expected failure* on the legacy app and a requirement for the Rust one.

| ID | Legacy behaviour | Required |
|---|---|---|
| D01 | A command the daemon rejects still prints nothing and exits 0 (IPC answers `OK` before acting). | Non-zero exit, message on the caller's stderr. |
| D02 | Invalid names / URLs / unknown providers are ignored; error goes to the daemon's invisible stderr. | Refused up front with exit 2. |
| D03 | Any `http://` URL accepted → session sent in cleartext to remote hosts. | `http` only for loopback (see V03). |
| D04 | `--idle-timeout abc`: argparse error is swallowed and **the window pops up**. | Exit 2, no side effect. |
| D05 | Unknown flags, typos, abbreviations (`--tog`), stray words are ignored (and show the window). | Exit 2. |
| D06 | A payload above 4096 bytes is truncated and still answered `OK`. | Refused (size limits, exit 2). |
| D07 | Login cookies and cache created `0755/0644`, socket `0755`, state `0644`. | Everything `0700/0600`. |
| D08 | A corrupt `providers.json` is silently replaced by defaults + the new entry. | Kept as `providers.json.corrupt*`. |
| D09 | When the WebKit web process dies the view stays blank forever. | Reloaded automatically. |
| D10 | One idle connection blocks every other command for ~1 s (serial listener, `recv` timeout). | No head-of-line blocking (< 500 ms). |
| D11 | After a network failure the recovery loads the provider *root*, not the page you were on (reproduced with a real click, F5 offline, network back). Needs `LW_SLOW=1` (~40 s). | Come back to the same URL. |
| D12 | `--quit` with no daemon boots GTK/WebKit and creates the data directories first. | Instant, silent, no side effects. |
| D13 | **Every** command shows the window afterwards, including `--hard-clean`, `--soft-clean`, `--idle-timeout`, `--user-agent`, `--add-provider`, `--remove-provider`. | Only `--new-chat` and `--provider` show it; the others leave visibility alone (section 2). |

---

## 5. Deliberate deviations (for your review)

The Rust implementation keeps the functionality and the way of using it; where it differs in
behaviour, usually for security, the difference is listed here. All are in code and tested; each
can be reverted by editing one function.

| ID | Deviation | Why |
|---|---|---|
| V01 | Unknown options, unique-prefix abbreviations, stray positionals → exit 2 (legacy ignored them). | D05; abbreviations break when an option is added. |
| V02 | Invalid values are reported by the client at parse time. | D02, D04, D06. |
| V03 | **`http://` only for `localhost`, `127.0.0.0/8`, `::1`.** Credentials in the URL, non-ASCII hosts, backslashes, `%` in hosts, and numeric hosts that a WHATWG parser reads differently (`https://2130706433/` is 127.0.0.1) are refused. | The session cookie must not travel in cleartext; one string must mean one thing to every parser. **A self-hosted UI on a LAN IP over plain http no longer works** — say if you need an opt-in. |
| V04 | `--user-agent`: printable ASCII, 1–512 bytes (legacy: anything; empty meant "no override"). | No header/control-sequence injection. |
| V05 | `--idle-timeout`: ASCII integer, at most 525 600 (legacy accepted `1_0`, ` 5 `, Unicode digits, 10¹¹ — which overflows GLib's timer). | Strict input. |
| V06 | `--quit` with no daemon is a no-op. | D12. |
| V07 | `-h/--help`, `-V/--version` exist. | Usability. |
| V08 | New framed binary protocol on a **versioned socket name** (`.v1.sock`). | The legacy daemon answers `OK` to *anything*: bytes it cannot parse are read as stray command-line arguments, ignored, and the default action (show the window) runs. Binary frames sent to it would pop the window up and report success. Distinct names keep incompatible versions from ever talking. |
| V09 | The client refuses an unsafe runtime dir / socket (symlink, wrong owner, group/other access). | See `security.md`. |
| V10 | The daemon is started from the file named `llm-wrapperd` next to the client, never through `PATH`, and only if it and every parent are trustworthy. | Binary planting. |

Planned for later phases and **not implemented yet** (decided earlier, listed so nobody assumes
otherwise): data under XDG with `0700/0600` (D07), the memory policy (only the active view stays
warm; hidden-idle daemons release the web process — measured −75 % memory), the compat profile
for User-Agent/stealth scripts, GTK4 + WebKitGTK 6.0.

---

## 6. Baseline (legacy), for comparison

`rust/scripts/e2e.sh --bench` → `docs/baseline-legacy.json`. Conditions: Xvfb with software
rendering, Intel Xeon 2.1 GHz × 4, WebKitGTK 2.52.6, a tiny fake chat page on loopback. **Absolute
numbers are not what a laptop with a GPU shows; compare implementations under the same conditions.**
Three runs agreed within the spread shown.

| Metric | Legacy |
|---|---|
| Cold start (command → state reports the provider), p50 | 252–278 ms |
| Hotkey: command spawned → state shows new visibility, p50 / p99 | 10.0–10.6 ms / 14.6–19.3 ms |
| Memory while hidden (PSS): UI / web / network / **total** | 75 / 78 / 27 / **181–182 MiB** |
| Memory while visible, total | 191–193 MiB |
| CPU while hidden (10 s) | 0.0–0.2 % |

The hotkey figure includes the harness's own 3 ms poll granularity and process spawn with pipes; it
is meaningful only relative to another implementation measured the same way. In isolation, the
Rust client costs 1.9–2.1 ms per call against 6.6–7.2 ms for the bash+`nc` launcher (same mock
daemon, 1000–1500 calls per round, three rounds); an empty static Rust program costs 1.5–1.7 ms.

---

## Not covered by automated tests

Honest list; each needs a manual check or a better harness:

* Closing the window with the window-manager button (`delete-event` → hide). X11 `xdotool` here
  has no `WM_DELETE_WINDOW` command and `windowclose` kills the client, so this cannot be driven
  without a window manager.
* External links opening in the default browser, the upload file chooser, downloads and their
  notifications (portal dialogs).
* The visual effect of zoom keys, `F12` Web Inspector.
* Real providers (ChatGPT, Gemini, Claude): unreachable from the sandbox; everything runs
  against a fake page. Login flows, Cloudflare/Turnstile behaviour and `content-visibility` on real
  DOMs are untested.
* Wayland: window class verified by hand (above); the suite itself runs on X11.
* The `DBUS_SYSTEM_BUS_ADDRESS` power-saver workaround, GPU behaviour, voice mode (not used).
* Behaviour on WebKitGTK versions other than 2.52.6.
