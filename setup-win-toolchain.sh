#!/bin/bash
# Eenmalig: bouwomgeving voor build-win.sh opzetten op Ubuntu (Linux → Windows).
# Ubuntu haalt de Windows-afhankelijkheid uit de std-broncode en levert de
# opstartbestanden rsbegin.o/rsend.o niet mee; dit script zet ze terug.
set -e
V=1.97
apt-get install -y -q rustc-$V cargo-$V rust-$V-src mingw-w64 libasound2-dev pkg-config libgtk-3-dev libxkbcommon-dev
R=/usr/lib/rust-$V
L=$R/lib/rustlib/src/rust/library
grep -q 'windows-link' $L/std/Cargo.toml || printf '\n[target.%s.dependencies.windows-link]\npath = "../windows_link"\n' "'cfg(windows)'" >> $L/std/Cargo.toml
D=$R/lib/rustlib/x86_64-pc-windows-gnu/lib
mkdir -p $D/self-contained
for f in rsbegin rsend; do
  RUSTC_BOOTSTRAP=1 $R/bin/rustc --edition 2021 --target x86_64-pc-windows-gnu --emit=obj -C panic=abort -C opt-level=2 \
    --crate-type lib -o $D/self-contained/$f.o $L/rtstartup/$f.rs
  cp $D/self-contained/$f.o $D/
done
echo "Klaar. Bouwen: ./build-win.sh --bin studiorecorder"
