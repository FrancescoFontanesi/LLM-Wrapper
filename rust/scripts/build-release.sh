#!/usr/bin/env bash
# Release build.
#
# The `llm-wrapper` client is linked statically (static-pie): it then starts without the dynamic
# loader, has no runtime library dependency at all, and costs ~0.5 ms less per hotkey press
# (measured: 1.9-2.1 ms against 2.3-2.6 ms dynamic, with an empty static Rust program at 1.5-1.7 ms).
# The GTK/WebKit daemon cannot be static, so each is built by itself.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

TARGET="$(rustc -vV | sed -n 's/^host: //p')"
OUT="${1:-dist}"
mkdir -p "$OUT"

RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" \
    cargo build --release --locked --target "$TARGET" -p llm-wrapper-ctl
install -m 0755 "target/$TARGET/release/llm-wrapper" "$OUT/llm-wrapper"

echo "built:"; ls -l "$OUT"
file "$OUT/llm-wrapper" | sed 's/, BuildID.*//'
