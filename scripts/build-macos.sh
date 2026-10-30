#!/usr/bin/env bash
# Run locally on a macOS build machine, never on a Linux host.
set -euo pipefail
[[ "$(uname -s)" == Darwin ]] || { echo 'This script requires macOS and Xcode Command Line Tools.' >&2; exit 1; }
cd "$(dirname "$0")/.."
command -v node >/dev/null || { echo 'Install Node.js LTS first.' >&2; exit 1; }
command -v cargo >/dev/null || { echo 'Install stable Rust with rustup first.' >&2; exit 1; }
xcode-select -p >/dev/null
npm ci --ignore-scripts
cargo check --manifest-path src-tauri/Cargo.toml --locked
# Ad-hoc signing by default. Developer signing/notarization requires separate
# separate credentials/configuration; this script never installs or stores them.
npm run tauri -- build --bundles app,dmg "$@"
