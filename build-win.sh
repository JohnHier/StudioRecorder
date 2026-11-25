#!/bin/bash
# Cross-compile StudioRecorder for Windows (x86_64-pc-windows-gnu) from Linux.
export PATH=/usr/lib/rust-1.97/bin:$PATH
export RUSTC_BOOTSTRAP=1
cd "$(dirname "$0")"
cargo build --release -Zbuild-std=std,panic_abort --target x86_64-pc-windows-gnu "$@"
# Kleinere exe zonder debug-symbolen (eenmalig opzetten: ./setup-win-toolchain.sh)
T=${CARGO_TARGET_DIR:-target}/x86_64-pc-windows-gnu/release
for e in "$T"/studiorecorder.exe "$T"/archiefrecorder.exe; do [ -f "$e" ] && x86_64-w64-mingw32-strip "$e"; done; true
