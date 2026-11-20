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

`Cargo.toml` declares the version to use for the next release tag.

## Preparing a release

1. **Version:** confirm `workspace.package.version` is the version to release, or bump it
   through a normal PR.
2. **Blockers:** the release checklist items for the release are done. For the first workflow-built release these include
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
   git tag -a v0.1.1 -m "clipuh v0.1.1"
   ```

7. **Push** only that tag: `git push origin v0.1.1`. Pushing the tag is what starts the
   release workflow; merging PRs never releases.
8. **Watch** the release workflow run: tag/version validation, checks, Windows build,
   publication.
9. **Check the artifacts** on the GitHub Release: download the ZIP, verify the SHA-256,
   unzip and start `clipuh.exe` on a clean machine, check the version shown in the sidebar.

### The existing 0.1.0 prerelease

`v0.1.0` is a lightweight tag on `a7c45d9` (PR #53). A GitHub prerelease "clipuh v0.1.0" was
published for it on 2026-11-15 with a manually attached `clipuh-v0.1.0-windows-x64.exe`, before
the automated release workflow and this changelog existed.

Because a release was published from it, do not move or recreate the `v0.1.0` tag. The next
release, including the first one built by the automated workflow, needs a new version: bump
`workspace.package.version` (for example to `0.1.1` or `0.2.0`) through a normal PR, then
follow the steps above. Replacing the existing prerelease instead (deleting both the release
and the tag) is possible but is a deliberate decision for the repository owner.
