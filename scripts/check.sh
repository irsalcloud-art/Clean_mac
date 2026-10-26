#!/bin/sh
# Runs the full MacClean verification suite. POSIX sh, so it can be invoked
# from Rush, zsh, bash or CI alike: `sh scripts/check.sh`.
set -eu
cd "$(dirname "$0")/.."
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    if rustup target list --installed 2>/dev/null | grep -qx "$target"; then
        cargo build --release --target "$target"
        echo "built target/$target/release/macclean"
    else
        echo "skipped $target (not installed: rustup target add $target)"
    fi
done
