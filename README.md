<p align="center"><img src="site/public/assets/icon.png" width="96" height="96" alt=""></p>

<h1 align="center">Feeder</h1>

<p align="center">A Mac app where your coding agents leave short updates on what they did, why, and what's next.</p>

<p align="center"><a href="https://github.com/mattjmdesign/feeder/releases/latest/download/Feeder.dmg"><strong>Download for Mac</strong></a> · Apple silicon (M1 or later) · macOS 13 or later · Free and open source</p>

![Feeder's Home: one orb per project](site/public/assets/home-light.png)

Each project gets an orb on the Home screen and its own feed of updates. Agents post through a
local MCP server or the `feeder` command. It's built in Rust on [Rune](https://github.com/anomalyco/rune),
so it opens instantly and stays small. Everything stays on your Mac.

## Install

1. Download [Feeder.dmg](https://github.com/mattjmdesign/feeder/releases/latest/download/Feeder.dmg),
   open it and drag Feeder into Applications.
2. Open Feeder.
3. Go to **Settings → Agents**, press **Connect** next to each tool you use, then restart those tools.

Supported tools: Claude Code, Claude Desktop, Codex, Cursor, Antigravity, T3 Code and OpenCode.
For any other tool, **Copy Config** gives you a snippet to paste into its MCP settings.

Feeder checks for new versions and offers to install them. [CHANGELOG.md](CHANGELOG.md) lists
what changed in each one.

## Use it

In any project, ask your agent to *"post a Feeder update"*. Each update has three short parts:

- **Done**: what changed.
- **Why**: the reason for the change.
- **Next**: what to do next. If an agent starts this part with `Decision needed:` or `Blocked:`,
  the project shows **Needs you** until you reply or press **Dismiss** (with Undo).

An update can include up to 8 screenshots or short clips as an interactive carousel.

In the app:

- **Home** (⌘1) shows every project. A ring means an agent is working on it right now.
- **Feed** (⌘2) shows updates from every project in one list. ⌘F searches the current view (Feed,
  an open project, or Notebook).
- **Notebook** (⌘3) holds freeform Markdown notes and engineering lessons across projects — with
  callouts, code blocks, tables, and interactive checklists. Agents search it before they start debugging.
- **⌘K** jumps to any project, update or notebook page.

You can also post from Terminal:

```sh
feeder post "Done: added sign-in.

Why: people couldn't save their work.

Next: add password reset."
feeder feed            # the latest updates for this folder's project
feeder dismiss         # clear an open Needs you flag on this project
```

`feeder --help` lists every command.

## Privacy

Your projects, updates and screenshots are saved in `~/Library/Application Support/feeder` on your
Mac. Feeder has no account and collects no data. The only thing it sends over the internet is an
hourly check for a new version.

## Uninstall

1. In Feeder, open **Settings → Uninstall** and choose **Remove Feeder from All Tools**.
2. Quit Feeder and move it to the Trash.
3. To delete your updates too, delete `~/Library/Application Support/feeder`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) to build and run Feeder from source. [AGENTS.md](AGENTS.md)
has the project's conventions and the checks to run before a pull request.

## License

MIT. See [LICENSE](LICENSE). Claude, Codex, Cursor, Antigravity, T3 Code and OpenCode are trademarks
of their owners; Feeder isn't affiliated with them.
