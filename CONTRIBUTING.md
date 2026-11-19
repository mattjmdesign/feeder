# Contributing to Feeder

Thanks for helping. Feeder is a small Mac app, so changes stay small too: one fix or feature per
pull request, with the checks passing.

## What you need

- A Mac with Apple silicon and the Xcode command line tools (`xcode-select --install`).
- [rustup](https://rustup.rs). The toolchain is pinned in `rust-toolchain.toml` and installs
  itself on the first build.
- [pnpm](https://pnpm.io), only if you change the website in `site/`.

## Build and run

```sh
FEEDER_HOME="$(mktemp -d)" cargo run -p feeder-app   # the app, on empty scratch data
cargo run -p feeder-cli -- --help                    # the `feeder` command
pnpm --dir site install && pnpm --dir site dev       # the website
```

Point `FEEDER_HOME` at a scratch folder, never at your real data. `scripts/site/demo-data.sh
<empty folder>` fills one with made-up projects if you want something to look at.

## Before you open a pull request

1. Run the checks listed under **Commands** in [AGENTS.md](AGENTS.md). They are the same
   ones the maintainer runs; CI only runs them when started by hand.
2. For anything you can see, include a screenshot from the scratch data, in light and dark.
3. Describe what changed for someone using Feeder, in plain words.

[AGENTS.md](AGENTS.md) also covers where things live, the UI conventions and the rules that keep
build output off your disk. It's written for coding agents, and it's the reference for people too.

## Don't commit

- Personal data: real project names, paths, emails, tokens or screenshots of real work.
- Build output (`target/`, `dist/`, `site/dist/`) or built apps and disk images.
- Changes to `Cargo.lock` that aren't part of a dependency change you meant to make.

## Security issues

Report them privately; see [SECURITY.md](SECURITY.md).

## Releases

Only the maintainer can publish a release: it needs a signing certificate and keys that live on
their Mac. If your change should be in the release notes, say so in the pull request.
