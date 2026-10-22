# llm-wrapper — Rust workspace

The Rust rewrite of the Python wrapper in the repository root, built to be its final code base.
Work proceeds in phases; the legacy app keeps working untouched until the Rust one reaches parity.

| Phase | Status |
|---|---|
| 0 — executable specification of the legacy behaviour, baseline measurements | done |
| 1 — wire protocol, validation, command-line client | done |
| 2 — GTK4 + WebKitGTK 6.0 daemon | not started |
| 3–5 — providers/navigation, memory policy, packaging | not started |

```
crates/protocol   wire format, validation, CLI grammar — no dependencies, parses untrusted bytes
crates/ctl        the `llm-wrapper` command (client; starts the daemon when none runs)
crates/e2e        black-box suite + fake chat site + benchmark; drives any implementation
scripts/          build-release.sh (static client), e2e.sh (suite / benchmark under Xvfb)
../docs/          spec.md, wire-protocol.md, security.md, baseline-legacy.json
```

## Build, test, check

```bash
cargo test --workspace                 # unit + integration; e2e tests skip without a display
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
scripts/build-release.sh [DIR]         # static-pie `llm-wrapper` client, built with --locked
scripts/e2e.sh                         # the spec against the legacy app (xvfb-run, dbus-daemon, python3-gi, WebKitGTK 4.1)
scripts/e2e.sh --impl rust --rust-bin DIR
scripts/e2e.sh --bench out.json
```

* Rust **1.80** or newer for the shipped crates (`rust-version`, checked: 1.79 fails).
* `LW_STRICT=1` turns every "skipped: prerequisite missing" in the e2e suite into a failure — use it
  in CI. `LW_SLOW=1` adds the 40-second network-recovery scenario. `LW_KEEP=1` keeps each test's
  sandbox directory for inspection.
* Quality gates: `unsafe` is forbidden, clippy `all` and `pedantic` are errors, production crates
  have no third-party dependencies. Details and the proof per control: `../docs/security.md`.

## Licence

The repository has no `LICENSE` file yet and the crates are marked `publish = false`. Choosing a
licence is a decision for the owner; nothing here assumes one.
