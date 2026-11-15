#!/usr/bin/env bash
# Validates a release tag against the version Cargo builds clipuh with.
#
#   scripts/release/check-version.sh <tag> [<cargo-version>]
#
# Accepted tags: vMAJOR.MINOR.PATCH, optionally -alpha.N, -beta.N or -rc.N
# (no leading zeros, no build metadata). The tag without its "v" must equal
# the version of the clipper-app package exactly. Without <cargo-version> it
# is read from `cargo metadata` (the workspace version clipuh.exe reports).
#
# On success prints `version=...` and `prerelease=true|false`, and appends
# them to $GITHUB_OUTPUT when that is set. Exits non-zero on any mismatch.
set -euo pipefail

tag="${1:-}"
if [[ -z "$tag" ]]; then
  echo "usage: $0 <tag> [<cargo-version>]" >&2
  exit 2
fi

num='(0|[1-9][0-9]*)'
pattern="^v${num}\.${num}\.${num}(-(alpha|beta|rc)\.${num})?$"
if [[ ! "$tag" =~ $pattern ]]; then
  echo "error: '$tag' is not a release tag (expected vMAJOR.MINOR.PATCH or vMAJOR.MINOR.PATCH-{alpha,beta,rc}.N)" >&2
  exit 1
fi
version="${tag#v}"

cargo_version="${2:-}"
if [[ -z "$cargo_version" ]]; then
  cargo_version="$(cargo metadata --format-version 1 --no-deps --locked |
    jq -r '.packages[] | select(.name == "clipper-app") | .version')"
fi
if [[ -z "$cargo_version" || "$cargo_version" == "null" ]]; then
  echo "error: could not read the clipper-app version from cargo metadata" >&2
  exit 1
fi
if [[ "$version" != "$cargo_version" ]]; then
  echo "error: tag $tag does not match the Cargo version $cargo_version (workspace.package.version in Cargo.toml)" >&2
  exit 1
fi

prerelease=false
[[ "$version" == *-* ]] && prerelease=true

echo "version=$version"
echo "prerelease=$prerelease"
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  {
    echo "version=$version"
    echo "prerelease=$prerelease"
  } >> "$GITHUB_OUTPUT"
fi
