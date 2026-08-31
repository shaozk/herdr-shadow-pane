#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
cargo build --release
mkdir -p bin
cp target/release/herdr-shadow-pane bin/herdr-shadow-pane
xattr -c bin/herdr-shadow-pane 2>/dev/null || true
codesign --force --sign - bin/herdr-shadow-pane 2>/dev/null || true
