# Releasing clipuh

Releases are built and published by [`.github/workflows/release.yml`](../.github/workflows/release.yml)
in [jona1502/Clipuh](https://github.com/jona1502/Clipuh). Only pushing a version tag releases;
merging a pull request never does, and CI never creates or pushes tags.

## Executable

The `clipper-app` package builds one binary, `clipuh` (`clipuh.exe` on Windows). The
internal package and crate names stay `clipper-*`.

```text
cargo run -p clipper-app --bin clipuh                          # debug build, with console
cargo build --release --locked -p clipper-app --bin clipuh     # target/release/clipuh.exe
```

Renaming the executable does not move any user data: settings, logs, the clip library and
the single-instance lock are keyed by the app name, not by the executable file name. Only a
"Start with Windows" entry written by a build that was still named `clipper-app.exe` points
at the old file; the setting then shows as off and turning it on again rewrites it.

## Cutting a release

1. Set `workspace.package.version` in `Cargo.toml` to the new version (for example `0.1.1`)
   and merge that change to `main` through a normal PR.
2. Tag the merged commit on `main` and push the tag:

   ```text
   git switch main && git pull --ff-only
   git tag -a v0.1.1 -m "clipuh v0.1.1"
   git push origin v0.1.1
   ```

3. The workflow then:
   - checks the tag format, that the tag points at a commit on `main`, and that the tag
     equals `v` + the Cargo version of `clipper-app` (a mismatch fails the run; nothing
     edits `Cargo.toml` for you);
   - runs the same Format, Clippy and Tests jobs as PR CI (`ci.yml`, reused through
     `workflow_call`) on the tagged commit;
   - builds `cargo build --release --locked -p clipper-app --bin clipuh` on `windows-latest`
     and packages it with [`scripts/release/package.ps1`](../scripts/release/package.ps1);
   - waits for the `release` environment (if it has required reviewers), verifies the
     checksum and creates the GitHub Release with `gh release create --verify-tag`.

   Publication is skipped if any earlier job fails. It also fails, without changing
   anything, if a release for the tag already exists.

`v0.1.0` is already taken: a prerelease with a manually attached executable was published
for it on 2026-11-15, before this workflow existed. Do not move that tag; the first release
built by this workflow needs a new version.

### Accepted tags

`vMAJOR.MINOR.PATCH` (`v0.1.0`, `v0.1.1`, `v1.0.0`) and prereleases
`vMAJOR.MINOR.PATCH-alpha.N`, `-beta.N`, `-rc.N`, which are published as GitHub
prereleases. Leading zeros, build metadata (`+…`) and other suffixes are rejected.
[`scripts/release/check-version.sh`](../scripts/release/check-version.sh) does the check;
`scripts/release/check-version.test.sh` tests it and runs in every release workflow run.

### Assets

| File | Contents |
| --- | --- |
| `clipuh-vX.Y.Z-windows-x64.zip` | `clipuh.exe` |
| `clipuh-vX.Y.Z-windows-x64.zip.sha256` | `<sha256>  clipuh-vX.Y.Z-windows-x64.zip` (`sha256sum -c` format) |

`clipuh.exe` is self-contained: icons, sounds and the window icon are compiled in, and Media
Foundation, Direct3D 11 and WASAPI are part of Windows. The packaging script checks that the
executable is a 64-bit GUI-subsystem image and that the archive holds exactly the expected
files. The ZIP keeps the executable name stable (`clipuh.exe`) while the download name
carries the version.

The checksum detects corrupted downloads. It is not a signature: builds are **not
code-signed** yet, which is a separate release task.

## Dry runs

PRs that touch the release workflow, `ci.yml`, `scripts/release/` or the `clipper-app`
manifest/build script run the workflow up to the packaged artifact (uploaded to the run for
14 days), using the version currently in `Cargo.toml`. The publish job never runs for PRs.
A real end-to-end publication has not been exercised yet; the first test should be a
prerelease tag (for example `v0.1.1-rc.1` with that version in `Cargo.toml`) that can be
deleted afterwards.

## Hardware validation

GitHub-hosted runners have no GPU, monitor or audio devices, so tests that need them are
`#[ignore]`d and do not gate a release. Before tagging, run on a real Windows machine:

```text
cargo test --workspace -- --ignored
```

and smoke-test the packaged `clipuh.exe`: it starts without a console window, shows the
clipuh icon, a second start activates the running window instead of starting another
instance, replay and recording work, and logs appear in `%LOCALAPPDATA%\clipuh\logs`.

## Repository settings (recommended)

- **Protected tags:** add a tag ruleset for `v*` that restricts creation, update and
  deletion to maintainers, so nobody else can trigger a release or move a published tag.
- **`release` environment:** add required reviewers so each publication waits for an
  explicit approval, and restrict its deployment refs to `v*` tags.
- **Branch protection on `main`:** require the CI checks, so release tags always point at
  reviewed, green commits.
- **Actions:** third-party actions in `release.yml` are pinned to full commit SHAs; the
  default `GITHUB_TOKEN` permission is read-only, and only the publish job, which runs no
  repository code, gets `contents: write`. No personal access token or secret is used.
- Rust build caches are only restored from `main` and the release's own tag scope, never
  from pull requests.

## Private repository

Clipuh is currently a private repository. Release assets there are only downloadable by
signed-in users with access, so they are not a public distribution channel, and the app
must never embed GitHub credentials to fetch them. Public downloads and the future updater
will likely need a separate public binary-release repository (or another host); that is a
separate decision, as is changing the repository's visibility.

## Extending the pipeline

The jobs are split so later release work slots in without reshaping the workflow:

- **Code signing:** sign `clipuh.exe` in the `build` job before `package.ps1`, and the
  installer after it, with the key held in the `release` environment.
- **Velopack:** add a step after the release build that runs `vpk pack` on the staged
  payload and uploads the installer and update packages with the other assets; the
  native update manager is app-side work and is not part of this pipeline.
- **Public distribution:** add a publish target next to `publish`, again gated by the
  `release` environment.
