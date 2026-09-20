#!/bin/sh
# Usage: cargo.sh <built-binary> <meson-output> <cargo> [cargo args...]
# Runs `cargo build` with the given options and copies the produced binary
# to where meson expects it.
set -eu

built="$1"
output="$2"
cargo_bin="$3"
shift 3

"$cargo_bin" build "$@"
cp "$built" "$output"
