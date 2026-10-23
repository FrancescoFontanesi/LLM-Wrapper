#!/usr/bin/env bash
# Run the black-box end-to-end suite (or the benchmark) under a private X server.
#
#   scripts/e2e.sh                      # legacy Python app (Phase 0 spec; needs python3 + PyGObject + WebKitGTK 4.1)
#   scripts/e2e.sh --impl rust --rust-bin DIR    # Rust client/daemon found in DIR
#   scripts/e2e.sh --slow               # also run the ~40 s offline-recovery scenario
#   scripts/e2e.sh --bench [OUT.json]   # baseline measurements instead of tests
#   scripts/e2e.sh -- <args for cargo test>
#
# Needs: xvfb-run, dbus-daemon, cargo. xdotool + xprop enable the keyboard/window-class tests, and
# dbus-monitor (package dbus-bin) the check that a finished download is announced.
# cargo has no "skipped" state, so the summary below lists every SKIP explicitly.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

impl=legacy rust_bin="" bench="" slow=""
while [ $# -gt 0 ]; do
    case "$1" in
        --impl) impl="$2"; shift 2 ;;
        --rust-bin) rust_bin="$2"; shift 2 ;;
        --slow) slow=1; shift ;;
        --bench) bench="${2:-}"; [ -n "$bench" ] && [[ "$bench" != --* ]] && shift; shift || true; [ -n "$bench" ] || bench=-; ;;
        --) shift; break ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

need() { command -v "$1" >/dev/null 2>&1 || { echo "e2e: '$1' is required ($2)" >&2; exit 1; }; }
need xvfb-run "apt install xvfb"
need dbus-daemon "apt install dbus"
for t in xdotool xprop; do
    command -v "$t" >/dev/null 2>&1 || echo "e2e: note: '$t' not found; keyboard and window-class tests will be skipped" >&2
done

export LW_IMPL="$impl"
[ -n "$slow" ] && export LW_SLOW=1
if [ "$impl" = rust ]; then
    [ -n "$rust_bin" ] || rust_bin="$PWD/target/release"
    export LW_RUST_BIN="$rust_bin"
    [ -x "$LW_RUST_BIN/llm-wrapper" ] || { echo "e2e: $LW_RUST_BIN/llm-wrapper not found; build with scripts/build-release.sh" >&2; exit 1; }
else
    for py in python3.12 python3.11 python3.13 python3; do
        if command -v "$py" >/dev/null && "$py" -c 'import gi' 2>/dev/null; then export LW_PYTHON="$(command -v "$py")"; break; fi
    done
    [ -n "${LW_PYTHON:-}" ] || { echo "e2e: no python3 with PyGObject (apt install python3-gi gir1.2-gtk-3.0 gir1.2-webkit2-4.1)" >&2; exit 1; }
fi

xvfb=(xvfb-run -a -s "-screen 0 1280x1600x24")

if [ -n "$bench" ]; then
    cargo build --release -p llm-wrapper-e2e --bin bench >/dev/null
    out=(); [ "$bench" != - ] && out=(--out "$bench")
    exec "${xvfb[@]}" ./target/release/bench "${out[@]}"
fi

log="$(mktemp "${TMPDIR:-/tmp}/lw-e2e.XXXXXX.log")"
status=0
"${xvfb[@]}" cargo test -p llm-wrapper-e2e "$@" -- --test-threads=3 --nocapture >"$log" 2>&1 || status=$?

echo "== implementation: $impl   (full log: $log)"
grep -E '^test .* \.\.\. (ok|FAILED)$' "$log" | sed 's/^test //' | sort
echo
skips=$(grep -c '^SKIP:' "$log" || true)
echo "== skipped: $skips"
{ grep '^SKIP:' "$log" || true; } | sort | uniq -c | sed 's/^/   /'
echo "== known defects confirmed (expected failures on legacy):"
{ grep '^xfail ' "$log" || true; } | sort | sed 's/^/   /'
echo
{ grep -E '^test result:' "$log" || true; } | sed 's/finished in.*//' | sort | uniq -c
if [ "$status" -ne 0 ]; then echo "== FAILED (exit $status); first failures:"; grep -E -A6 '^---- ' "$log" | head -60; fi
exit "$status"
