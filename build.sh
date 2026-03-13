#!/bin/bash
set -e

# 1. Install Rust Toolchain if not present
if ! command -v cargo &> /dev/null; then
    echo "Cargo not found. Installing Rust toolchain..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
fi

# 2. Ensure worker-build is installed
if ! command -v worker-build &> /dev/null; then
    echo "worker-build not found. Installing..."
    cargo install -q worker-build
fi

# 3. Build the worker
echo "Building worker..."
worker-build --release
