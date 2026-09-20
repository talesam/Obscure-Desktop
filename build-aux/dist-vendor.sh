#!/bin/sh
# Called by `meson dist`: vendors all crates into the tarball so distro
# packagers can build offline (`meson setup -Doffline=true`).
set -eu

export DIST="$1"
export SOURCE_ROOT="$2"

cd "$SOURCE_ROOT"
mkdir "$DIST"/.cargo
cargo vendor --locked "$DIST"/vendor > "$DIST"/.cargo/config.toml
# Move vendor into dist tarball directory
sed -i 's/^directory = ".*"/directory = "vendor"/g' "$DIST"/.cargo/config.toml
