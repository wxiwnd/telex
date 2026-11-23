#!/usr/bin/env sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is not installed" >&2
    echo "install Rust first: https://rustup.rs" >&2
    exit 1
fi

cd "$root"
cargo install --path . --locked --force

if command -v tlx >/dev/null 2>&1; then
    echo "installed: $(command -v tlx)"
else
    echo "installed to Cargo's bin directory; add it to PATH if needed"
fi
