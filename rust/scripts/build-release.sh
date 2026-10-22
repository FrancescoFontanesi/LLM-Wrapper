#!/usr/bin/env bash
# Release build.
#
# The `llm-wrapper` client is linked statically (static-pie): it then starts without the dynamic
# loader, has no runtime library dependency at all, and costs ~0.5 ms less per hotkey press
# (measured: 1.9-2.1 ms against 2.3-2.6 ms dynamic, with an empty static Rust program at 1.5-1.7 ms).
# The GTK/WebKit daemon cannot be static, so each is built by itself, `llm-wrapperd` dynamically.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

TARGET="$(rustc -vV | sed -n 's/^host: //p')"
OUT="${1:-dist}"
mkdir -p "$OUT"

RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" \
    cargo build --release --locked --target "$TARGET" -p llm-wrapper-ctl
install -m 0755 "target/$TARGET/release/llm-wrapper" "$OUT/llm-wrapper"

# The daemon links GTK 4 and WebKitGTK 6.0 (libgtk-4-dev, libwebkitgtk-6.0-dev, Rust 1.89+).
cargo build --release --locked --target "$TARGET" -p llm-wrapper-daemon
install -m 0755 "target/$TARGET/release/llm-wrapperd" "$OUT/llm-wrapperd"

echo "built:"; ls -l "$OUT"
for f in llm-wrapper llm-wrapperd; do
    file "$OUT/$f" | sed 's/, BuildID.*//'
done
echo "The client looks for llm-wrapperd next to itself (never on \$PATH): keep the two together."
