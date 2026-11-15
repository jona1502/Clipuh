# Versioning

## Single source of truth

The application version is `workspace.package.version` in the root
[`Cargo.toml`](../Cargo.toml). Every crate in `crates/` declares `version.workspace = true`, so
there is exactly one number to change. Everything else derives it at build time from
`CARGO_PKG_VERSION`:

| Consumer | How it gets the version |
| --- | --- |
| Sidebar version label (`v0.1.0`) | `clipper-ui` `version_label()`; a unit test checks it against `Cargo.toml` |
| Startup log line and debug-log export | `env!("CARGO_PKG_VERSION")` in `clipper-app` / `clipper-ui` |
| Release workflow (separate PR) | `cargo metadata` version of `clipper-app`, compared with the tag |
| Windows `VERSIONINFO` resource (P0.4, not implemented) | should be generated in `crates/clipper-app/build.rs` from `CARGO_PKG_VERSION_*` |
| Installer / updater (Velopack, not implemented) | should take the version from the same `cargo metadata` value as the release workflow, e.g. `vpk pack --packVersion <version>` |

Do not add version constants anywhere else. `website/package.json` has its own npm version for
the website and is not the app version.

## Scheme

[Semantic Versioning](https://semver.org/). While the major version is 0, a minor bump
(`0.2.0`) may contain breaking changes such as settings-format changes; patch releases
(`0.1.1`) contain fixes only. Prereleases use `-alpha.N`, `-beta.N` or `-rc.N`
(`0.2.0-rc.1`).

Git tags are the version with a `v` prefix: `v0.1.0`, `v0.2.0-rc.1`. A release tag must point
at a commit on `main` whose `Cargo.toml` has exactly that version.

The first public version is **0.1.0** (tag `v0.1.0`). `Cargo.toml` already declares it.

## Preparing a release

1. **Version:** confirm `workspace.package.version` is the version to release, or bump it
   through a normal PR.
2. **Blockers:** the release checklist items for the release are done. For 0.1.0 these include
   the project license decision and asset provenance ([licensing.md](licensing.md)), shipping
   `THIRD_PARTY_NOTICES.md` with the binary, and the release workflow itself.
3. **Checks:** CI is green on `main`, and the hardware tests passed on a real Windows machine:
   `cargo test --workspace -- --ignored`, plus a manual smoke test of the release build
   (replay save, recording, editor export, second start activates the running window).
4. **Changelog:** move the entries from `[Unreleased]` in [CHANGELOG.md](../CHANGELOG.md)
   into the version's section, add the release date, update the compare links, and merge that
   PR.
5. **Merged:** every PR meant for the release is merged into `main`.
6. **Tag** the merged commit deliberately, as an annotated tag:

   ```text
   git switch main
   git pull --ff-only
   git tag -a v0.1.0 -m "clipuh v0.1.0"
   ```

7. **Push** only that tag: `git push origin v0.1.0`. Pushing the tag is what starts the
   release workflow; merging PRs never releases.
8. **Watch** the release workflow run: tag/version validation, checks, Windows build,
   publication.
9. **Check the artifacts** on the GitHub Release: download the ZIP, verify the SHA-256,
   unzip and start `clipuh.exe` on a clean machine, check the version shown in the sidebar.

### Existing `v0.1.0` tag

A lightweight `v0.1.0` tag already exists on `origin`. It points at `a7c45d9` (PR #53) and was
created before the release workflow and this changelog existed. Check on GitHub whether a
Release is attached to it. Tag pushes are what trigger releases, so before releasing 0.1.0
the owner has to decide deliberately between:

- deleting the existing tag (`git push origin :refs/tags/v0.1.0`, plus locally
  `git tag -d v0.1.0`) and recreating it on the final release commit with the steps above, or
- leaving it and releasing the first public build under a new version (for example `0.1.1`).

Never move or recreate a tag after a release was published from it.
