#!/bin/bash
# Cross-compile StudioRecorder for Windows (x86_64-pc-windows-gnu) from Linux.
export PATH=/usr/lib/rust-1.91/bin:$PATH
export RUSTC_BOOTSTRAP=1
cd "$(dirname "$0")"
cargo build --release -Zbuild-std=std,panic_abort --target x86_64-pc-windows-gnu "$@"
