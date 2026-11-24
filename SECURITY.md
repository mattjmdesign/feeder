# Security

## Reporting a vulnerability

Please report security issues privately through GitHub:
**[Report a vulnerability](https://github.com/mattjmdesign/feeder/security/advisories/new)**.
Don't open a public issue for them.

Include what you found, how to reproduce it, and which version of Feeder you ran (Feeder → About
Feeder). You'll get a reply within a week. Fixes ship as a normal release, which installed copies
pick up within an hour.

## What Feeder does on your Mac

These are the parts worth checking when you review it:

- **Data.** Everything lives in `~/Library/Application Support/feeder` (`0700` permissions): a
  SQLite database, copied media (symlinks are refused and image/video headers are validated), and
  `settings.json`. Updates, replies, project briefs, and library pages reject common API keys and
  tokens before saving.
- **Curator.** When you start a session, the selected CLI sends your messages and the sources
  it reads to the model provider it uses. Ask sessions can
  read only Library pages in scope. Review sessions can also read flags and Feed threads in
  scope and propose cited Library edits for you to apply or revert. Each CLI manages its own credentials and reuses your
  existing sign-in; Feeder stores none. Curator uses a subscription only after you turn it on in
  Settings → Curator. Antigravity's Curator settings live in Feeder's data directory
  (`--gemini-dir`), so Feeder never edits `~/.gemini`. Curator strips shell API keys and
  disables personal hooks, built-in tools, and personal MCP servers. Provider-managed policies may
  still apply.
- **Network.** The app checks `github.com/mattjmdesign/feeder/releases` for updates about once an
  hour. `feeder mcp` speaks MCP over stdin and stdout and opens no port. `feeder serve` (an
  optional companion web page, off unless you run it) listens only on the address you give it,
  enforces `Host`/`Origin` checks, caps request header and body sizes, sets a strict CSP, and
  checks the bearer token you set in constant time.
- **Other apps' files.** Only when you press Connect or Disconnect, or run `feeder setup`, Feeder
  edits those tools' MCP settings (for example `~/.cursor/mcp.json` or `~/.codex/config.toml`) and
  writes two skill files into their skills folders. Each edited file is first copied to
  `<file>.feeder-backup` (`0600` permissions).
- **Notifications.** Posted as Feeder through macOS's UserNotifications, after you allow them.
  Notification clicks only open `feeder://` URLs.

## Releases and updates

Releases are built on the maintainer's Mac by `scripts/release.sh`:

- **Signing:** the app, its helpers and the disk image are signed with a Developer ID certificate,
  with the hardened runtime and no extra entitlements.
- **Notarization:** Apple notarizes the disk image, and the ticket is stapled to it.
- **Update feed:** updates go through Sparkle. The feed (`appcast.xml`) and every disk image are
  signed with an EdDSA key whose public half is built into the app, and Sparkle verifies an update
  before extracting it.
