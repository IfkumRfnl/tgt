#!/usr/bin/env bash
# Idempotent Cloud Agent setup for tgt (a Rust TUI for Telegram).
#
# Prepares a fresh machine to build, test, lint and run tgt end to end:
#   1. System libraries required by the default build features
#      (static-download + voice-message + rodio).
#   2. The Rust toolchain pinned to the same version the project's Dockerfile
#      uses (1.91), with rustfmt and clippy.
#   3. A warm `cargo build` so the first interactive build is fast. This also
#      auto-downloads TDLib via the `download-tdlib` feature (no API keys or
#      manual TDLib compilation required).
#
# Safe to run repeatedly: apt, rustup and cargo are all incremental/idempotent.
set -euo pipefail

RUST_VERSION="1.91.0"

echo "==> [1/3] Installing system dependencies"
export DEBIAN_FRONTEND=noninteractive
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
  build-essential \
  pkg-config \
  cmake \
  clang \
  libc++-dev \
  libc++abi-dev \
  libasound2-dev \
  libssl-dev \
  zlib1g-dev \
  libopus-dev \
  gperf \
  git \
  curl

echo "==> [2/3] Ensuring Rust ${RUST_VERSION} toolchain (matches project Dockerfile)"
rustup toolchain install "${RUST_VERSION}" --profile minimal -c rustfmt -c clippy
rustup default "${RUST_VERSION}"

echo "==> [3/3] Warming build (default features: static-download, voice-message, rodio)"
# TDLib is fetched and statically linked automatically by the default feature set.
cargo build

echo "==> Setup complete"
rustc --version
cargo --version
