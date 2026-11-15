#!/bin/bash
set -euo pipefail

# 1. Install Rust Toolchain if not present
if ! command -v cargo &> /dev/null; then
    echo "Cargo not found. Installing Rust toolchain..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi

# 2. Ensure wasm32 target is installed
if ! rustup target list --installed | grep -qx 'wasm32-unknown-unknown'; then
    echo "Installing wasm32-unknown-unknown target..."
    rustup target add wasm32-unknown-unknown
fi

# 3. Ensure worker-build ^0.9 is installed (optimized via --locked)
if ! command -v worker-build &> /dev/null || [[ "$(worker-build --version 2>/dev/null || true)" != 0.9.* ]]; then
    echo "worker-build (^0.9) not found. Installing..."
    cargo install -q worker-build --version "^0.9" --locked --force
fi

# 4. Build the worker
echo "Building worker..."
cd worker && worker-build --profile worker-release
