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

- **Data.** Everything lives in `~/Library/Application Support/feeder`: a SQLite database, copied
  screenshots, and `settings.json`. Nothing is uploaded.
- **Network.** The app checks `github.com/mattjmdesign/feeder/releases` for updates about once an
  hour. `feeder mcp` speaks MCP over stdin and stdout and opens no port. `feeder serve` (an
  optional companion web page, off unless you run it) listens only on the address you give it and
  requires the token you set.
- **Other apps' files.** Only when you press Connect or Disconnect, or run `feeder setup`, Feeder
  edits those tools' MCP settings (for example `~/.cursor/mcp.json` or `~/.codex/config.toml`) and
  writes two skill files into their skills folders. Each edited file is first copied to
  `<file>.feeder-backup`.
- **Notifications.** Posted as Feeder through macOS's UserNotifications, after you allow them.

## Releases and updates

Releases are built on the maintainer's Mac by `scripts/release.sh`:

- **Signing:** the app, its helpers and the disk image are signed with a Developer ID certificate,
  with the hardened runtime and no extra entitlements.
- **Notarization:** Apple notarizes the disk image, and the ticket is stapled to it.
- **Update feed:** updates go through Sparkle. The feed (`appcast.xml`) and every disk image are
  signed with an EdDSA key whose public half is built into the app, and Sparkle verifies an update
  before extracting it.
