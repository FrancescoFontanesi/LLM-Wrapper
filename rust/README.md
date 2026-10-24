# llm-wrapper — Rust workspace

The Rust rewrite of the Python wrapper in the repository root, built to be its final code base.
Work proceeds in phases; the legacy app keeps working untouched until the Rust one reaches parity.

| Phase | Status |
|---|---|
| 0 — executable specification of the legacy behaviour, baseline measurements | done |
| 1 — wire protocol, validation, command-line client | done |
| 2 — GTK4 + WebKitGTK 6.0 daemon (`llm-wrapperd`): window, control socket, XDG storage, single instance | done — the whole e2e suite passes against it |
| 3 — navigation policy, compatibility profile (User-Agent, JavaScript environment), permissions, pop-ups, downloads/uploads, notifications | done — one profile for all providers, see `../docs/spec.md` V15–V21 |
| 4 — memory policy: a hidden, idle page goes back to the provider's home page (instant to show, no conversation kept), nothing typed is lost | done — see `../docs/spec.md` V22 and the phase 4 table in section 6 |
| 5 — packaging, retirement of the Python app | not started |

```
crates/protocol   wire format, validation, CLI grammar — no dependencies, parses untrusted bytes
crates/platform   runtime directory checks, socket client, private files — no dependencies
crates/ctl        the `llm-wrapper` command (client; starts the daemon when none runs)
crates/daemon     `llm-wrapperd`: GTK4 + WebKitGTK 6.0 window, control socket, state, navigation policy (gtk4, webkit6, serde_json)
crates/e2e        black-box suite + fake chat site + benchmark; drives any implementation
scripts/          build-release.sh (static client + daemon), e2e.sh (suite / benchmark under Xvfb)
../docs/          spec.md, wire-protocol.md, security.md, baseline-legacy.json
```

## Build, test, check

```bash
cargo test --workspace                 # unit + integration; e2e tests skip without a display
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
scripts/build-release.sh [DIR]         # static-pie `llm-wrapper` client + `llm-wrapperd`, built with --locked
scripts/e2e.sh                         # the spec against the legacy app (xvfb-run, dbus-daemon, python3-gi, WebKitGTK 4.1)
scripts/e2e.sh --impl rust --rust-bin DIR
scripts/e2e.sh --bench out.json
```

* Rust **1.80** or newer for `protocol`, `platform` and `ctl` (`rust-version`, checked: 1.79 fails);
  **1.89** for the daemon (`File::try_lock`, and the gtk-rs 0.10 bindings).
* The daemon needs the GTK 4 and WebKitGTK 6.0 development packages to build (`libgtk-4-dev`,
  `libwebkitgtk-6.0-dev`; developed and tested with GTK 4.14.5 and WebKitGTK 2.52.6, older versions
  unverified), and `bubblewrap` + `xdg-dbus-proxy` at run time so that WebKit can sandbox its web
  process.
* Running the daemon by hand: put `llm-wrapper` and `llm-wrapperd` in the same directory and run
  `llm-wrapper --show`. The client never looks for the daemon anywhere else.
* `LW_STRICT=1` turns every "skipped: prerequisite missing" in the e2e suite into a failure — use it
  in CI. `LW_SLOW=1` adds the 40-second network-recovery scenario and the idle-page-goes-home scenarios (`s19`–`s24`; `--idle-timeout` cannot be shorter than a minute, so about three minutes in all). `LW_KEEP=1` keeps each test's
  sandbox directory for inspection.
* If `/.flatpak-info` exists on the host (inside a flatpak, or left behind as a mount point by
  bubblewrap run as root — it appeared on the build machine during phase 4), GTK believes it is
  sandboxed: it sets no `_NET_WM_PID` on its windows and may use the desktop portal for file
  dialogs. The suite copes (windows are then found by class or title), but a person at a desk would
  find window rules by pid broken; the file is not something the app creates.
* Quality gates: `unsafe` is forbidden in every crate of this workspace, clippy `all` and `pedantic`
  are errors, and the client side (`protocol`, `platform`, `ctl`) has no third-party dependencies.
  Details and the proof per control: `../docs/security.md`.
* Some daemon unit tests need root (`ipc::tests::a_connection_from_another_user_is_refused` runs a
  client as uid 65534) and say `SKIP` otherwise.

## Licence

MIT, free to use, copy, modify and distribute: see `../LICENSE`. The crates stay marked
`publish = false`; publishing them is a separate decision.
