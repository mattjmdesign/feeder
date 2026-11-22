# Feeder: working rules for agents

Feeder is a macOS app where coding agents post short updates (Done, Why, Next) to a feed per
project, plus a shared Notebook of lessons. This file is how to change it without breaking the
app, the user's data or the development Mac's disk. People contributing start at
[CONTRIBUTING.md](CONTRIBUTING.md); the rules here apply to them too.

## Commands

Run all of these before handing work back, in this order:

```sh
cargo fmt --all
cargo clippy --locked --workspace --all-targets -- -D warnings
FEEDER_SPARKLE_DIR="$(scripts/fetch-sparkle.sh)" cargo clippy --locked -p feeder-app --all-targets -- -D warnings
cargo test --locked --workspace
xcrun swiftc -typecheck crates/feeder-menubar/FeederMenuBar.swift   # if you touched the menu bar item
pnpm --dir site build                                               # if you touched site/
scripts/changelog.sh --check                                        # if you touched release-notes/
```

The second clippy run is the only one that compiles `updater.rs`, which needs Sparkle. These local
runs are the checks: `.github/workflows/checks.yml` runs only when started by hand.

Run the app or the CLI on scratch data, never the real store in `~/Library/Application Support/feeder`:

```sh
export FEEDER_HOME="$(mktemp -d)" && scripts/site/demo-data.sh "$FEEDER_HOME"   # made-up projects
cargo run -p feeder-app
cargo run -p feeder-cli -- --help
pnpm --dir site dev
```

## Where things live

| Path | What it is |
| --- | --- |
| `crates/feeder-core` | The store (SQLite), the Notebook and agent setup (`setup` edits each tool's MCP config) |
| `crates/feeder-core/src/gardener` | The Notebook gardener: runs the person's own `claude` or `codex` as a live session wired only to `feeder mcp --gardener` |
| `crates/feeder-cli` | The `feeder` command and the MCP server (`feeder mcp`) |
| `crates/feeder-app` | The window, in [Rune](https://github.com/anomalyco/rune). `updater.m` wraps Sparkle, `notify.m` posts notifications |
| `crates/feeder-app/src/style.rs` | Colours, type roles and corner radii. Use these; don't write sizes or weights inline |
| `crates/feeder-app/src/main.rs` | App state, the poll loop and the snapshot scenes |
| `crates/feeder-menubar` | The menu bar item: one Swift file, compiled by `scripts/mac-app.sh` |
| `site/` | The Astro website. Pushing `site/**` to `main` deploys it to GitHub Pages (`.github/workflows/pages.yml`) |
| `site/src/demo` | The animated product demos on the site, built on the Stageshow kit in `site/src/stageshow/kit` |
| `release-notes/<version>.md` | What's new in each version. `CHANGELOG.md` is generated from these |
| `scripts/` | `release.sh`, `mac-app.sh`, `changelog.sh`, the disk image window (`dmg/`), licenses and site helpers |

## Rules you'd otherwise get wrong

### Privacy

- This repo is public. Never commit personal data: real project names, paths, emails, tokens or
  screenshots of real work. Every image in the site and README comes from
  `scripts/site/demo-data.sh`.
- Don't open, copy or snapshot the real database to check something. Use `FEEDER_HOME`.
- Don't run plain `scripts/mac-app.sh` on a Mac with a release build installed. It installs a
  local build with no update feed and takes that Mac off the updater. It refuses unless
  `FEEDER_REPLACE_RELEASE=1` is set.

### App UI

- **Title Case for every action**: buttons, menu items and links that do something ("Copy
  Context", "Mark All Read", "Show Data in Finder"). Short words like *a*, *to*, *in*, *for* stay
  lowercase, as in Apple's guidelines. Headings, status text and sentences stay sentence case.
- **A heading stands alone.** No count pill next to it and no tagline under it. The content
  already shows how many there are.
- **One place per action.** Don't add a button that repeats something already on the same screen
  or one click away (for example, Show in Finder and Pin live on the project's orb menu, so the
  project header doesn't have them). A page header holds only actions about that page.
- **Inner corners follow the window.** macOS draws a 16 pt continuous window corner. A pane inset
  from the window edge uses `style::pane_kit`, not `16 − inset`, which looks pinched.
- **Type comes from `style.rs`**: five sizes (24, 16, 14, 13, 12) and two weights (400, and 500
  as `style::MEDIUM`). Nothing bolder than 500.
- Interface copy is plain and practical: say what it does for the person using it. No pitch, no
  internals.

### Website

- Type uses the `--fs-*` tokens in `site/src/styles/global.css` and weights 400 and 500 only.
- Import Stageshow kit modules directly (`../stageshow/kit/stage`), not `kit/index.ts`, which
  pulls in the phone styles. The kit's brand logos were removed; tool logos come from
  `site/src/demo/ToolLogo.tsx`.
- Check a demo with a quick browser screenshot at desktop and phone widths. Don't run Stageshow's
  filming pass.
- Copy is for someone deciding whether to download Feeder. Show the product, and leave release
  mechanics (signing, notarization, feeds) out.

### Disk

The development Mac's disk fills up fast. These aren't optional.

- **One build folder.** Use the repo's `target/`. Don't set `CARGO_TARGET_DIR`, pass
  `--target-dir` or make git worktrees for this repo: each grows its own multi-GB `target/`. If
  you must use a worktree, remove it with `git worktree remove` when done.
- **No release builds to check things.** `cargo build --release`, `cargo test --release` and
  `cargo bench` each add a second full build. Release builds happen only in `scripts/release.sh`
  and `mac-app.sh --package`.
- **Keep `[profile.dev.package."*"] debug = false`** in `Cargo.toml`. Without it dependency debug
  info took 13 GB. A clean build, test and lint run leaves `target/` near 2 GB, and `mac-app.sh`
  clears `target/` past 8 GB.
- **No build matrices.** No `--all-features`, extra targets or nightly toolchains. Feeder builds
  for `aarch64-apple-darwin` on the toolchain in `rust-toolchain.toml`. After changing that file,
  run `rustup toolchain uninstall <old version>` unless another project pins it.
- **`dist/` holds one release.** `release.sh` deletes older images after publishing. Don't copy
  DMGs anywhere else.
- **Clean up temp files.** Snapshots go to a temp or scratch folder, never the repo. Headless
  Chrome started with `--user-data-dir="$(mktemp -d)"` leaves about 200 MB per run: delete the
  profile in an `EXIT` trap and stop Chrome after 60 seconds.
- **Tests stay unit-sized**, in `#[cfg(test)]` modules next to the code, using temp dirs each test
  removes. No fixtures, recorded media or databases in the repo.

### Dependencies

- Rune is pinned to a git commit in `Cargo.toml`. Bump it together with every `[patch]` entry that
  shares its `rev`. Its features are trimmed to `accessibility`, `metal-backend` and `svg`; don't
  turn its defaults back on.
- `serde_json` in `feeder-core` keeps `preserve_order`, so `setup` keeps the key order of the
  config files it edits.
- After `cargo update` or a version bump, run every check and commit `Cargo.lock`.
- After adding or upgrading a dependency, regenerate `assets/ThirdPartyLicenses.html` with
  `scripts/licenses/generate.sh` (needs `cargo install cargo-about --features cli`). It refuses
  licenses missing from `scripts/licenses/about.toml`; only add one that allows binary
  distribution.
- The app ships no third-party artwork beyond Simple Icons (CC0) marks in `logos.rs`. Other tools
  show their installed app's own icon.

## Verification

Show a UI change working before you hand it back. Headless snapshots render the real view from
`FEEDER_HOME`:

```sh
target/debug/feeder-app --snap "$TMPDIR/home.png" --size 1180x760 --scale 2 --frames 20 --theme light
target/debug/feeder-app --snap "$TMPDIR/nb.png" --scene notebook --size 1180x760 --scale 2 --frames 20 --theme dark
```

- Home is the default; other scenes are `feed`, `notebook`, `notebook-page`, `notebook-new`,
  `settings` and `app-icon` (see `scenes` in `main.rs`).
- `--frames` gives background work, such as the Settings tool scan, time to land.
- Check light and dark. Look at the image; a snapshot that renders isn't proof the layout is
  right.
- Window chrome (the traffic lights, the real corner) only shows in a live window: run the app and
  capture it with `screencapture -l <window id>`.

## Ship a release

Releases need the maintainer's go-ahead every time. Pushing to `main` releases nothing.

1. Bump `version` in `Cargo.toml` and run `cargo update -w`.
2. Write `release-notes/<version>.md` under a `## What's new` heading, in plain words for the
   people using Feeder. Run `scripts/changelog.sh` to rebuild `CHANGELOG.md`.
3. Run every check, commit, then run `scripts/release.sh`. It builds, signs, notarizes, signs the
   update feed and publishes `Feeder.dmg` and `appcast.xml` as a GitHub release on this repo. It
   also posts `appcast.xml` to `mattjmdesign/feeder-releases`, the feed 0.2 and 0.3 installs still
   read. Installed copies offer the update within an hour.

The Developer ID certificate, the `feeder` notarytool profile and the Sparkle key named `feeder`
are only in the maintainer's keychain. The disk image's window comes from `scripts/dmg/` (edit
`background.html`, then run `render-background.sh`). Site screenshots come from demo data,
snapshotted, with window buttons added by `scripts/site/window-buttons.py`.
