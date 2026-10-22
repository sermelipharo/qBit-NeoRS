#!/bin/sh
# Installs the pinned Rust toolchain on a GitHub-hosted runner.
set -eu
TOOLCHAIN=1.99.0
if ! command -v rustup >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
  echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
  PATH="$HOME/.cargo/bin:$PATH"
fi
rustup toolchain install "$TOOLCHAIN" --profile minimal --component clippy
rustup default "$TOOLCHAIN"
echo "RUSTUP_TOOLCHAIN=$TOOLCHAIN" >> "$GITHUB_ENV"
rustc --version
