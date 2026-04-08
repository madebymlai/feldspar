#!/usr/bin/env bash
set -euo pipefail

# Cross-compile feldspar for all npm platforms
# Prerequisites:
#   rustup target add x86_64-unknown-linux-gnu
#   rustup target add aarch64-unknown-linux-gnu
#   rustup target add x86_64-apple-darwin
#   rustup target add aarch64-apple-darwin
#   rustup target add x86_64-pc-windows-gnu

TARGETS=(
    "x86_64-unknown-linux-gnu:npm/linux-x64/bin/feldspar"
    "aarch64-unknown-linux-gnu:npm/linux-arm64/bin/feldspar"
    "x86_64-apple-darwin:npm/darwin-x64/bin/feldspar"
    "aarch64-apple-darwin:npm/darwin-arm64/bin/feldspar"
    "x86_64-pc-windows-gnu:npm/win32-x64/bin/feldspar.exe"
)

for entry in "${TARGETS[@]}"; do
    target="${entry%%:*}"
    output="${entry#*:}"
    echo "Building for $target..."
    cargo build --release --target "$target"
    mkdir -p "$(dirname "$output")"
    if [[ "$target" == *windows* ]]; then
        cp "target/$target/release/feldspar.exe" "$output"
    else
        cp "target/$target/release/feldspar" "$output"
    fi
    echo "  → $output"
done

echo "Done. Binaries placed in npm/<platform>/bin/"
