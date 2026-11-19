# Changelog

What changed in each version of Feeder, newest first. Each section is copied from
`release-notes/<version>.md` by `scripts/changelog.sh`; edit those files, not this one.

## 0.4.0

- **A new icon.** Marbles fill the tile, like the orbs on Home.
- **A quieter menu bar item.** It shows a small marble icon, with a dot only when a project is waiting on your reply. Its menu lists each project once: waiting on you, working now, then recent.
- **Connect your tools from the start.** On first run, Connect Your Tools opens Settings right at Agents.
- **Feeder is open source.** The code, website and releases now live at github.com/mattjmdesign/feeder.
- **Notifications come from Feeder.** They show Feeder's name and icon, and clicking one opens the project. macOS asks once whether to allow them.
- **Disconnect a tool.** Settings → Agents has a Disconnect button for each tool, and Settings → Uninstall takes Feeder out of every tool at once.
- **Smoother and smaller.** Scrolling glides more naturally, text stays sharp while it moves, and the download is about a quarter smaller.
- **About Feeder** links to the website, the source code and what Feeder does with your data.

## 0.3.0

- **A shorter list of tools.** Settings → Agents now covers Claude Code, Claude Desktop, Codex, Cursor, Antigravity, T3 Code and OpenCode, each with its own logo. Gemini CLI and Windsurf are gone.
- **OpenCode's app counts.** Feeder now finds OpenCode when only its Mac app is installed.
- **Updates arrive sooner.** Feeder checks for a new version every hour instead of once a day.

## 0.2.0

- **Connect only the tools you use.** Settings → Agents lists the coding tools on your Mac, shows which ones already reach Feeder, and connects each with one button. Another tool can use the MCP config you copy from there.
- **Feeder updates itself.** When a new version is out, an icon appears in the corner. Read what changed, then restart to update.
- **One typeface.** Feeder uses the macOS system font everywhere, with fewer sizes and lighter headings.
- **A cleaner notebook.** The hashtag row is gone, list rows line up with the heading, and pages open with a plain Back link.
- **Steady toolbar.** The Home, Feed and Notebook tabs stay in the same place when you switch views.
