#!/bin/bash
# Double-click this file in Finder to build (first time) and open Sapu.
cd "$(dirname "$0")" || exit 1
if ! command -v cargo >/dev/null 2>&1; then
  source "$HOME/.cargo/env" 2>/dev/null
fi
echo "Opening Sapu… (the first build takes a few minutes)"
cargo run --release
