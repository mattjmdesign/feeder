# Feeder: working rules for agents

Feeder is a macOS app (Rune UI) with a CLI and MCP server. The workspace has three crates:
`feeder-core` (store, notebook, agent setup), `feeder-cli` (the `feeder` binary and `feeder mcp`),
and `feeder-app` (the window, plus `updater.m` for Sparkle and `notify.m` for notifications).
`crates/feeder-menubar` is one Swift file. `docs/` is the website (GitHub Pages).

This repo is public. Never commit personal data: no real project names, paths, emails, tokens or
screenshots of real work. Site and README images come from `scripts/site/demo-data.sh`.

The disk on the development Mac fills up fast, so the rules below about builds and leftovers are
not optional.

## Check a change

Run these before handing work back, in this order:

```sh
cargo fmt --all
cargo clippy --locked --workspace --all-targets -- -D warnings
FEEDER_SPARKLE_DIR="$(scripts/fetch-sparkle.sh)" cargo clippy --locked -p feeder-app --all-targets -- -D warnings
cargo test --locked --workspace
```

The second clippy run compiles `updater.rs`, which only builds with Sparkle. CI is turned off;
these local runs are the checks.

## Try a change

- Run the app with `cargo run -p feeder-app`. Point it at a copy of the data, not the real
  database: `FEEDER_HOME=<scratch dir> cargo run -p feeder-app`.
- For screenshots, use headless snapshots and write them to a scratch or temp folder, never the
  repo: `FEEDER_HOME=<scratch dir> target/debug/feeder-app --snap <out.png> --scene notebook --size 1180x760 --scale 2 --frames 20`.
  Scenes are listed in `main.rs` (`notebook`, `notebook-page`, `feed`, `settings`, …). `--frames`
  gives background work, such as the Settings tool scan, time to land.
- Don't run plain `scripts/mac-app.sh` on a Mac that has a release build installed. It installs a
  local build with no update feed, which takes that Mac off the updater. The script refuses unless
  `FEEDER_REPLACE_RELEASE=1` is set.

## Keep the disk clean

- **One build folder.** Use the repo's `target/` only. Don't set `CARGO_TARGET_DIR`, don't pass
  `--target-dir`, and don't make git worktrees for this repo: each one grows its own multi-GB
  `target/`. If you must use a worktree, delete it with `git worktree remove` when done.
- **No release builds to check things.** `cargo build --release`, `cargo test --release` and
  `cargo bench` each add a second full build. Release builds happen only inside
  `scripts/release.sh` (and `mac-app.sh --package`).
- **Debug builds stay small.** `[profile.dev.package."*"] debug = false` in `Cargo.toml` leaves
  dependency debug info out (it was 13 GB). Keep it. A clean build, test and lint run leaves
  `target/` near 2 GB, and `mac-app.sh` clears `target/` past 8 GB.
- **Don't add build matrices.** No `--all-features` builds, extra targets or nightly
  toolchains. Feeder builds for `aarch64-apple-darwin` on the toolchain pinned in
  `rust-toolchain.toml`.
- **Old toolchains go.** After changing `rust-toolchain.toml`, run
  `rustup toolchain uninstall <old version>` unless another project on the Mac pins it.
- **`dist/` holds one release.** `release.sh` deletes older images after publishing, because
  every published DMG lives on the releases page. Don't copy DMGs anywhere else.
- **Clean up temp files you create.** A script that starts headless Chrome with
  `--user-data-dir="$(mktemp -d)"` leaves about 200 MB per run in `$TMPDIR`. Delete the profile in
  an `EXIT` trap and stop Chrome after 60 seconds. The Feeder notebook has the full pattern
  (search "headless Chrome").
- **Tests stay unit-sized.** Put tests in `#[cfg(test)]` modules next to the code, using temp
  dirs that each test removes. No fixtures, recorded media or databases committed to the repo.

## Dependencies

- Rune is pinned to a git commit in `Cargo.toml`. Bump it together with every `[patch]` entry
  that points at the same repo (they share the `rev`). Its features are trimmed to the ones
  Feeder uses (`accessibility`, `metal-backend`, `svg`); don't turn its defaults back on.
- `serde_json` in `feeder-core` keeps `preserve_order`: `setup` edits users' config files and
  must keep their key order.
- After `cargo update` or a version bump, run the full check above. Commit `Cargo.lock`.
- After adding or upgrading a dependency, regenerate `assets/ThirdPartyLicenses.html` with
  `scripts/licenses/generate.sh` (needs `cargo install cargo-about --features cli`). It refuses
  licenses not listed in `scripts/licenses/about.toml`; don't add one without checking it allows
  binary distribution.
- Feeder ships no third-party artwork beyond Simple Icons (CC0) marks in `logos.rs`. Other tools
  show their installed app's own icon.

## Ship a release

1. Bump `version` in `Cargo.toml`, run `cargo update -w`, and write `release-notes/<version>.md`
   under a `## What's new` heading, in plain words for the people using Feeder.
2. Commit, then run `scripts/release.sh`. It builds, signs, notarizes, signs the update feed and
   publishes a GitHub release on this repo with `Feeder.dmg` and `appcast.xml`. Installed copies
   offer the update within an hour, and the website's Download button serves the new image.

The disk image's window (background, icon positions) comes from `scripts/dmg/`; edit
`background.html` and run `render-background.sh` to change it. For new site screenshots, fill a
scratch folder with `scripts/site/demo-data.sh`, snapshot it, and add the window buttons with
`scripts/site/window-buttons.py`.

Pushing to `main` releases nothing. The Developer ID certificate, the `feeder` notarytool profile
and the Sparkle key named `feeder` live only in this Mac's keychain.
