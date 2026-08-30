#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
cargo build --release
mkdir -p bin
cp target/release/herdr-shadow-pane bin/herdr-shadow-pane
