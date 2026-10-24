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
| `c` | `contract.rs` | Client-only contract; needs no daemon. |
| `k` | `keyboard.rs` | Real key/mouse input on X11 (keys, link clicks, the save and upload dialogs). Own binary: see the note in the file. |
| `s` | `daemon.rs` | What only the Rust daemon promises: hostile peers, one instance, private files, no planted links, clean shutdown, what a page may do, and the idle page going back to the provider's home page (`s19`–`s24`, slow: `LW_SLOW=1`, about three minutes). Skipped on legacy. |

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
| `--idle-timeout MIN` | Minutes hidden before the page goes back to the provider's home page (Rust; legacy: before the memory caches are dropped), default 15. Visibility unchanged. See V22. |
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

### Daemon behaviours specific to the Rust implementation

* The first command line of a daemon (the one that started it) is executed like any other: a bare
  start toggles (so a new daemon shows its window, as the bash launcher did); `--background` starts
  hidden. If that first command is refused, the daemon exits 1 and leaves nothing behind (`s07`).
* A second process started for an instance that is already running does not start a window: it
  hands its command over (`flock` decides who owns the instance, so simultaneous starts leave
  exactly one daemon: `s03`) and exits with that daemon's verdict.
* A command is answered only **after** it has been carried out, so a reply of `OK` is true; a
  refusal carries its reason (`d01`).
* A web process that crashes is reloaded (at most 3 times a minute), a page that failed because
  the network was down comes back on its own URL when the network returns or within 30 s (`d09`,
  `k02`/D11).
* Keys: Escape hides, Ctrl+N new chat, Ctrl+R / F5 reload, Ctrl+plus / Ctrl+minus / Ctrl+0 zoom
  (Shift allowed, `+` needs it on a US layout), F12 the Web Inspector — as in the README (`k01` types
  Escape, F5, Ctrl+R, Ctrl+Shift+R, Ctrl+N and the zoom keys; the Inspector is not driven, see the
  list at the end).
* `SIGTERM`/`SIGINT`/`SIGHUP` stop the daemon like `--quit`: socket and state file removed (`s08`).
  After `SIGKILL` the lock vanishes with the process and a stale socket is replaced (`s09`, `p12`).
* While no `DBUS_SYSTEM_BUS_ADDRESS` is reachable WebKit cannot read the power profile, which is how
  the legacy app avoids the 30 fps cap in power-saver mode; the Rust daemon sets the same variable
  before anything else. **Not verified on real power-saver hardware.**

### Behaviours kept on purpose

* **Q1** `--toggle --show` hides a visible window (toggle wins); `--hide --show` shows (`p02`).
* **Q2** `--quit` wins over any other option (`p10`).
* **Q3** A bare `llm-wrapper` toggles when a daemon runs (bash launcher), whereas `python app.py`
  with no arguments shows (from reading the code; not tested). Rust follows the launcher.

* **Q4** A link to another site opens in the default browser and the window stays where it was
  (`k03` for a plain link, `k04` for `target=_blank`: both pass on the legacy app and on Rust).
* **Q5** A link to `mailto:`, `file:` or an application scheme does nothing (`k05`, both).
* **Q6** A download asks where to save it, can be cancelled, and is announced when it finishes
  (`k07`, `k09`; the announcement is checked on Rust only, the legacy app runs `notify-send`).
* **Q7** Pages see a Chrome-on-Linux environment — `navigator.webdriver`, `vendor`, `window.chrome`,
  `userAgentData`, plugins — in every frame, popups included, and the containment style sheet
  applies (`p16`, identical on both; only the Chrome version differs, V15).

Dropped as not useful: the legacy habit of showing the window after *any* command. It was kept
only where it serves the user — `--new-chat` is bound to a key to start a chat you are about to
read, and the README documents that `--provider` shows the window — and removed for cache
cleaning and settings, where a window popping up is only a distraction (`--hard-clean` is bound to
a key of its own).

---

## 3. Files, state and identity

| Item | Legacy | Rust |
|---|---|---|
| State file (public API, read by Quickshell) | `$XDG_RUNTIME_DIR/llm-wrapper[-NAME].json` | **same name, same schema** |
| Socket | `$XDG_RUNTIME_DIR/llm-wrapper[-NAME].sock` | `llm-wrapper[-NAME].v1.sock` (see V08) |
| Login data / cache | `./session`, `./cache` (`instances/NAME/…`) in the repository | `$XDG_DATA_HOME/llm-wrapper`, `$XDG_CACHE_HOME/llm-wrapper` (`…/instances/NAME` for an instance); defaults `~/.local/share`, `~/.cache`; every directory `0700`, verified at each start |
| Provider list | `./providers.json` | `$XDG_CONFIG_HOME/llm-wrapper/providers.json` (`~/.config`), shared by all instances, `0600` |
| Single-instance lock | none (a socket probe) | `$XDG_RUNTIME_DIR/llm-wrapper[-NAME].lock`, an exclusive `flock` the kernel drops when the daemon dies |
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
(measured: plain `GtkWindow` + `prgname` keeps `llm-wrapper`; `p09` passes against the Rust daemon).

The existing login sessions and provider list of the legacy app are **not migrated** (they live in the
repository checkout): log in once again, and copy
`providers.json` to `$XDG_CONFIG_HOME/llm-wrapper/` if you added providers.

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
| D07 | Login cookies and cache created `0755/0644`, socket `0755`, state `0644`. | Nothing reachable by other users: the data, cache and config roots `0700`, socket, state, lock and provider list `0600`. (Files WebKit itself creates inside a root have WebKit's default mode, `0644`/`0755`; the `0700` root is what keeps them private, and `d07` checks reachability, not the mode of every file.) |
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
| V11 | **Camera/microphone (`enable_media_stream`) and encrypted media (DRM) are off** in the Rust daemon; the legacy app turned both on. | Voice mode is not used (your answer 5, confirmed again for phase 3): with them off the page can never ask for the microphone. Voice back would be one line in `ui.rs` plus a permission prompt. |
| V12 | Data, cache and config move to the XDG directories (see section 3); the sessions of the legacy app are not migrated. | D07; a repository checkout is not a place for logins. |
| V13 | An existing directory of ours with loose permissions is *tightened to `0700` and reported*; a symlink, a foreign owner, or an unreadable/unsafe provider list is *refused and set aside* (`providers.json.untrusted-<time>`), never followed. | Intransigent on planted links; forgiving on the user's own mistakes. |
| V14 | A daemon started by a command that the daemon then refuses (for instance an unknown `--provider`) exits 1 instead of staying up unnoticed. | D01: a refused command must be visible. |

| V15 | The default `User-Agent` and the brands the compatibility script reports say **Chrome 151** (reduced form `Chrome/151.0.0.0`); the legacy app said 131. One constant (`CHROME_FULL_VERSION` in `compat.rs`) drives both, so they cannot disagree. | A 2024 browser version is the kind of thing sites start refusing. 151 is the stable release I could confirm (Chrome Releases blog, 30 July 2026); later ones are published on a two-week cycle and I could not verify them. Update the constant when a site complains; `--user-agent` overrides it for a session. |
| V16 | **Links and navigations** follow the legacy rule (the provider's own site and the sign-in/bot-check domains stay in the window, everything else goes to the browser) with these differences, each pinned by the fixture generated from the legacy code (`rust/crates/daemon/tests/fixtures/gen_legacy_nav.py`): an IP-address provider matches only that address (legacy: any address ending in the same two numbers); a public suffix is not one site (`attacker.co.uk` is not `example.co.uk`; legacy compared the last two labels); a plain-`http` link to a site, and any link with a `user:password@`, goes to the browser instead of staying in the window; `chatgpt.com.` is `chatgpt.com`; an address that cannot be read unambiguously (backslashes, `%` or non-ASCII in the host, spaces, no host) is refused. | Legacy `is_internal` is the rule the app lives by, so it is kept; these are the places where it was too wide or where its parser and WebKit's disagree. |
| V17 | **Schemes**: only `http(s)`, `about:`, `blob:` and `data:` are ever navigated to; `file:`, `ftp:`, `mailto:`, `javascript:` and application schemes are refused, and nothing is launched for them. Legacy left them to the web view. | Handing arbitrary schemes to the desktop is how a web page starts programs. |
| V18 | **Windows a page opens**: a sign-in popup (an address that belongs to the provider or the trusted domains) opens in a window of its own that shares the session, as in legacy, at most four at a time; a window for another site goes to the browser **only after a click** (legacy: for any script, any time); everything else is refused. | A page must not be able to open browser tabs by itself, or flood the screen with windows. |
| V19 | **Permissions** (notifications, location, clipboard reading, device lists, pointer lock, storage access) are denied explicitly and logged once per kind; legacy denied them by not answering. Camera and microphone: see V11. | Same behaviour, but a decision instead of a default that a WebKit update could change. |
| V20 | **Downloads and uploads** go through GTK 4's file dialog (the desktop portal when there is one); one dialog at a time, further requests are refused; the name offered is the last component of what the server suggested, stripped of control characters; the page only ever receives the files the user picked. "Download complete/failed" are sent to the notification service over D-Bus (legacy ran `notify-send` through `PATH`). | A page asking for a hundred downloads gets one dialog; no helper program is looked up in `PATH`. |
| V21 | Not carried over: `allow_modal_dialogs` (legacy `True`), the hardware-acceleration policy (WebKitGTK 6 has none), and the WebGL/Web Audio/MediaSource switches (on by default in WebKit). | Modal page dialogs have no use here; the rest is default behaviour. |

| V22 | **A hidden page that has been idle for the idle timeout goes back to the provider's home page**: the web view loads the provider's own address (what opening the site in any browser shows: light, no conversation), so the next time the window is shown the home page is already there and nothing has to load. A conversation you left an hour ago is not reopened: you land on the home page, which is the intended use (the provider's own site keeps the conversation in its history). The legacy app only dropped the in-memory caches and kept the page. A daemon started hidden (`--background` at login) counts its idle time from the start; legacy never trimmed those at all. **Nothing typed is lost:** the page is left alone while it is loading, playing sound, has a pop-up or a file dialog open or a download running (looked at again every minute), and while it holds something typed or attached and not sent (text fields, text areas, `contenteditable` composers, password fields, a chosen file; asked in a JavaScript world of its own so the page cannot hide it; an answer the app cannot read keeps the page) — then it does what legacy did, drops the caches, and looks again after another idle period. A page that is already the provider's home page is not reloaded. | The memory a hidden page holds is the biggest cost of keeping the app resident (section 6), and a long conversation is what holds it. The page does not need to be kept: the provider's site loads only the latest part of a conversation and the older messages as you scroll up. To keep the legacy behaviour, give `--idle-timeout` a very large value (the maximum is 525 600 minutes). |
| V23 | A failed download no longer also announces "Download complete", and one the user cancelled is not announced at all. | Legacy connected both handlers to events that both fire on failure. |

What stays for later (decided earlier, listed so nobody assumes otherwise): packaging, and retiring
the Python app. The compatibility profile is **one profile for
every provider**: the legacy app had no per-provider differences and nothing known calls for any;
add them when a site gives a reason.

---

## 6. Baseline (legacy), for comparison

`rust/scripts/e2e.sh --bench` → `docs/baseline-legacy.json`. Conditions: Xvfb with software
rendering, Intel Xeon 2.1 GHz × 4, WebKitGTK 2.52.6, a tiny fake chat page on loopback. **Absolute
numbers are not what a laptop with a GPU shows; compare implementations under the same conditions.**
Three runs agreed within the spread shown.

| Metric | Legacy | Rust, phase 2 (release build; `baseline-rust-phase2.json`, one run) |
|---|---|---|
| Cold start (command → state reports the provider), p50 | 252–278 ms | 177 ms |
| Hotkey: command spawned → state shows new visibility, p50 / p99 | 10.0–10.6 ms / 14.6–19.3 ms | 6.8 ms / 13.0 ms |
| Memory while hidden (PSS): UI / web / network / **total** | 75 / 78 / 27 / **181–182 MiB** | 84 / 71 / 27 / **185 MiB** |
| Memory while visible, total | 191–193 MiB | 202 MiB |
| CPU while hidden (10 s) | 0.0–0.2 % | 0.0 % |

**The Rust column was measured on a 2.8 GHz Xeon, the legacy column on a 2.1 GHz one, so the time
figures are not like for like** (re-measure both on one machine before quoting a ratio). Memory is
not affected by the clock: **phase 2 is not lighter than the legacy app** (+3 MiB hidden, +10 MiB
visible, the UI process is larger under GTK4's software renderer). That is expected — the memory
policy (letting go of the page while hidden and idle) is phase 4, whose results are below, and
nothing in phase 2 was meant to reduce it. The saving so far is on the command side (1.9–2.1 ms
per hotkey call for the client against 6.6–7.2 ms for the bash launcher) and in the daemon's startup.

### Phase 4: a hidden page that has been idle (`baseline-phase4.json`)

Both implementations measured on the same machine this time (Xeon 2.8 GHz, Xvfb, software
rendering), a few seconds after the page loaded and again after `--idle-timeout 1` plus 80 s hidden.
"Light" is the small fake page used above; "heavy" holds about 150 MiB of JavaScript memory, as a
long conversation on a real site does (those measure in hundreds of MiB). PSS in MiB; in brackets the
part of it that is private heap (what only exiting gives back).

| | Legacy, light | Rust, light | Legacy, heavy | Rust, heavy |
|---|---|---|---|---|
| Hidden, page just loaded | 181 (59) | 190 (59) | 330 (209) | 341 (209) |
| **Hidden and idle** | 186 (60) | **146 (44)** | 334 (209) | **146 (43)** |
| Window visible after the command | 11 ms | 13 ms | 10 ms | 13 ms |
| Page requested after the command | — (never let go) | ~140–165 ms | — | ~140–165 ms |

* Legacy keeps everything it had; Rust ends with the UI process and the network process. On the
  heavy page that is **57 % of the memory attributed to the app (79 % of the private
  heap)**; on the light page the saving is small because there was little to give back — the benefit
  scales with the page.
* On a real site the number is the one that matters and **I could not measure it** (the sandbox
  reaches none); the heavy fake page stands in for it. Pages of long conversations are far larger
  than the light page, so the saving there is expected to be of the order of the heavy one or more.
* The UI process's PSS *rises* when the web process goes (84 → 113 MiB): the two share the pages of
  the WebKit libraries, and PSS hands a share to whoever remains. The total is the honest figure.
* Cold start and the rest: Rust starts in 152–177 ms against the legacy 252 ms, like for like.

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
* The **desktop-portal** versions of the file dialogs (the suite has no portal; the GTK 4 dialog
  is what runs), and a browser other than the fake one that stands in for the default.
* Real sign-in flows: popups and redirects of ChatGPT, Google, Apple and Microsoft are untested;
  the suite uses a fake site with a popup that behaves like one. Whether Chrome 151 (V15) is still
  accepted by the real sites is unknown.
* The visual effect of zoom keys, `F12` Web Inspector.
* Real providers (ChatGPT, Gemini, Claude): unreachable from the sandbox; everything runs
  against a fake page. Login flows, Cloudflare/Turnstile behaviour and `content-visibility` on real
  DOMs are untested.
* Wayland: window class verified by hand (above); the suite itself runs on X11.
* The `DBUS_SYSTEM_BUS_ADDRESS` power-saver workaround, GPU behaviour, voice mode (not used).
* Behaviour on WebKitGTK versions other than 2.52.6.
* Daemon: the enforcement of the memory-pressure thresholds, the
  refusal of non-web schemes (WebKit already refuses `http`→`file`, `javascript:` and `view-source:`
  from a page and does nothing with `mailto:`, so the daemon's own refusal cannot be told apart from
  outside — a mutation that lets them through survives the end-to-end suite; the unit tests cover it), the Web Inspector (F12), the peer-uid test when not run as root,
  and loading a page once with `--provider X --new-chat` (`s12` passes, but WebKit coalesces a
  repeated load, so the mutation "load twice" cannot be told apart either).
* Letting go of a page: whether the draft question is right for the composers of the **real** sites
  (ChatGPT, Claude, Gemini: ProseMirror and similar editors; only generic inputs are tested), the
  "playing sound" check (the sandbox has no audio), the zoom you set surviving a rebuild, and the
  real memory numbers (V22, section 6).
* The idle timer a daemon that starts hidden gets *by default* (15 minutes): the tests arm it with
  `--idle-timeout 1`, which goes through a different line, so the line that arms the default is
  unobserved (a mutation that removes it survives).

