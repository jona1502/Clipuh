#!/usr/bin/env bash
# Self-test for check-version.sh; needs no Rust toolchain.
#   scripts/release/check-version.test.sh
set -euo pipefail

check="$(dirname "$0")/check-version.sh"
failures=0

accept() { # <tag> <cargo-version> <expected prerelease>
  local out
  if ! out="$(GITHUB_OUTPUT= "$check" "$1" "$2" 2>&1)"; then
    echo "FAIL: expected '$1' (Cargo $2) to pass: $out"; failures=$((failures + 1)); return
  fi
  if ! grep -qx "prerelease=$3" <<<"$out"; then
    echo "FAIL: '$1' should report prerelease=$3: $out"; failures=$((failures + 1)); return
  fi
  echo "ok: accepts $1"
}

reject() { # <tag> <cargo-version>
  if GITHUB_OUTPUT= "$check" "$1" "$2" >/dev/null 2>&1; then
    echo "FAIL: expected '$1' (Cargo $2) to be rejected"; failures=$((failures + 1)); return
  fi
  echo "ok: rejects '$1' (Cargo $2)"
}

accept v0.1.0 0.1.0 false
accept v0.1.1 0.1.1 false
accept v0.2.0 0.2.0 false
accept v1.0.0 1.0.0 false
accept v10.20.30 10.20.30 false
accept v0.2.0-rc.1 0.2.0-rc.1 true
accept v1.0.0-beta.0 1.0.0-beta.0 true

# Malformed tags.
for tag in 0.1.0 v0.1 v0.1.0.0 v01.0.0 v0.01.0 v0.1.00 V0.1.0 v0.1.0- v0.1.0-rc \
  v0.1.0-rc.01 v0.1.0-dev.1 v0.1.0+build.1 v0.1.0-rc.1+b "v0.1.0 " " v0.1.0" \
  'v0.1.0;id' vx.y.z v-0.1.0 ''; do
  reject "$tag" "${tag#v}"
done

# Tag and Cargo version disagree.
reject v0.1.1 0.1.0
reject v0.1.0 0.1.1
reject v0.2.0 0.2.0-rc.1
reject v0.2.0-rc.1 0.2.0

if ((failures)); then
  echo "$failures check(s) failed"
  exit 1
fi
echo "all checks passed"
