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

# 3. Ensure worker-build is installed (optimized via --locked)
if ! command -v worker-build &> /dev/null; then
    echo "worker-build not found. Installing..."
    cargo install -q worker-build --locked
fi

# 4. Build the worker
echo "Building worker..."
cd worker && worker-build --profile worker-release
