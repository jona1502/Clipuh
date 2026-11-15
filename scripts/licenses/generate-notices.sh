#!/usr/bin/env bash
# Regenerates THIRD_PARTY_NOTICES.md from Cargo.lock with cargo-about.
#
#   scripts/licenses/generate-notices.sh          # rewrite the file
#   scripts/licenses/generate-notices.sh --check  # fail if it is out of date
#
# Needs cargo-about (version below). Runs offline from the crate sources that
# `cargo fetch` downloads, so the result depends only on Cargo.lock, about.toml
# and the template. Fails when a crate's license is missing, unreadable, or not
# in the accepted list in about.toml.
set -euo pipefail

CARGO_ABOUT_VERSION="0.9.3"

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

installed="$(cargo-about --version 2>/dev/null | awk '{print $2}')" || true
if [[ "$installed" != "$CARGO_ABOUT_VERSION" ]]; then
  echo "error: cargo-about $CARGO_ABOUT_VERSION is required (found: ${installed:-none})" >&2
  echo "       cargo install --locked --features cli cargo-about@$CARGO_ABOUT_VERSION" >&2
  exit 1
fi

cargo fetch --locked
out="$(mktemp)"
trap 'rm -f "$out"' EXIT
cargo-about generate --offline --locked --workspace --fail \
  -c about.toml -o "$out" scripts/licenses/about.hbs

if [[ "${1:-}" == "--check" ]]; then
  if ! diff -u THIRD_PARTY_NOTICES.md "$out"; then
    echo "error: THIRD_PARTY_NOTICES.md is out of date; run scripts/licenses/generate-notices.sh" >&2
    exit 1
  fi
  echo "THIRD_PARTY_NOTICES.md is up to date"
else
  mv "$out" THIRD_PARTY_NOTICES.md
  trap - EXIT
  echo "wrote THIRD_PARTY_NOTICES.md"
fi
