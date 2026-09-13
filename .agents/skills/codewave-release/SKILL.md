---
name: codewave-release
description: Cut a CodeWave release — run the full gate, bump the version everywhere, commit, and after explicit confirmation push the v* tag that triggers three-platform release CI. Use whenever the user wants to release, ship, or tag a CodeWave version, bump the version, or says 发版/发布/出新版本/打个tag/升级版本 — even a bare "release 0.3.0" or "发个版".
---

# CodeWave release

A release is: bump the version everywhere → commit on `main` → push main → push a `v*` tag. The tag push is what triggers `.github/workflows/release.yml`: it creates a **draft** Release, then three platform jobs (macOS / Linux / Windows) build via tauri-action and upload installers to it. Nothing auto-publishes — after CI finishes, a human reviews the assets and clicks **Publish** on the draft.

Pushing a new tag cancels an in-flight older release (workflow concurrency). The updater signing key (`TAURI_SIGNING_PRIVATE_KEY`, no password) is configured as a repo secret and the public key lives in `tauri.conf.json` — every release uploads signed updater artifacts (`*.sig` + `latest.json`), so **publishing a release makes it the live update source** for all installed apps. macOS code-signing/notarization (`APPLE_*`) remains optional: without it macOS artifacts are unsigned (first open needs right-click approval).

**This skill always stops before pushing.** The tag push ships the release and is effectively irreversible (artifacts are published, the version is consumed). Follow AGENTS.md: never push `main` or a tag without the user's explicit yes, even if every check is green.

## 1. Determine the version

- An explicit version in the invocation wins (accept `0.3.0` or `v0.3.0`).
- Otherwise derive a suggestion:
  - Latest tag: `git fetch --tags && git describe --tags --abbrev=0` (no tags yet → the tree version, `0.2.0`, is the floor).
  - What's shipping: `git log <latest-tag>..HEAD --oneline`
  - Suggest the next semver from those commits: breaking changes (`feat!` / `BREAKING CHANGE`) → bump the **minor** while the project is 0.x; `feat` → minor; `fix`/`chore`/`docs` → patch.
- State the suggestion and the commits it's based on, then ask the user to confirm or override. Never bump an unconfirmed version.
- Never reuse a version that already has a tag: installers carry the version and published releases are immutable. If a release went wrong, pick a new number, don't move the tag.

## 2. Pre-flight — abort on any failure

Run these before touching any file; a red tree never gets bumped.

1. `git status --porcelain` — must be empty. The bump rewrites 5 files; committing on a dirty tree mixes unrelated changes into the release commit.
2. `git branch --show-current` must be `main`, then `git pull --ff-only` — releases are always cut from an up-to-date main (GitHub Flow).
3. Full gate, in fail-fast order (mirrors CI 硬门槛; fmt/clippy 在 lint.yml 仍是 continue-on-error 软门槛，不阻塞发版):
   - `cargo test` (in `src-tauri/`; baseline: all green, 0 warnings)
   - `pnpm --dir ui test`
   - `pnpm --dir ui build` (tsc + vite build)

On failure: stop, show the failing output, and let the user decide what to fix. Do not bump.

## 3. Bump

```
pnpm bump <version>
```

The script strips a leading `v` itself. It rewrites `package.json` (root), `ui/package.json`, `src-tauri/tauri.conf.json`, and `src-tauri/Cargo.toml`, then refreshes `src-tauri/Cargo.lock` (`cargo update -w`, workspace members only) and `pnpm-lock.yaml`. Never edit versions by hand.

Verify with `git status --porcelain`: expect exactly **5 modified files** — `package.json`, `ui/package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` (pnpm-lock.yaml is refreshed too but doesn't change for a version-only bump — it doesn't record workspace package versions; on Windows with autocrlf a no-op bump may leave a phantom Cargo.lock entry whose `git diff` is empty). Anything else in the diff — investigate before committing.

## 4. Commit

```
git commit -m "chore(release): vX.Y.Z"
```

This matches the release-commit convention; no body needed.

## 5. Summarize, then STOP

Show the user, concretely:

- The version and the commits going out since the previous tag (or since the fork point for the first release)
- What release CI will build once the tag lands: a **draft** Release + Windows / macOS / Linux installers via tauri-action, plus signed updater artifacts (`*.sig` + `latest.json` — publishing the draft makes this version the live update source); macOS unsigned unless `APPLE_*` secrets are configured
- That the draft must be **published manually** after CI finishes — CI never releases on its own
- That pushing this tag cancels any in-flight older release

Then ask, and wait: "Push main + tag vX.Y.Z now?" A yes to the summary is consent to push; silence or anything ambiguous is not.

## 6. Push (only after the user's explicit yes)

```
git push origin main
git tag vX.Y.Z
git push origin vX.Y.Z
```

Then offer to monitor the release run (`gh run list --workflow=release.yml`, `gh run watch <run-id>`). After CI succeeds, remind the user to review and **Publish** the draft release at the GitHub Releases page.
