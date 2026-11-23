# Changelog

What changed in each version of Feeder, newest first. Each section is copied from
`release-notes/<version>.md` by `scripts/changelog.sh`; edit those files, not this one.

## 0.5.5

- **Curator for your Notebook.** Ask about anything in your Notebook or review flagged updates into durable notes with reviewable, revertible diffs. Pick your connected provider (Claude Code, Codex, OpenCode, or Antigravity), switch models or thinking levels mid-chat without losing the thread, and sign in directly from the app when a provider needs auth. Every model in the picker shows its service's logo on the left.
- **Notebook card grid with per-category duotones.** The Notebook index and category views now sit directly on the window background as a full-width responsive card grid (the white sheet appears when reading or editing a note). Each parent category generates its own unique monochromatic duotone dot pattern on the left of its cards so topics are scannable at a glance.
- **Simpler Notes & sidebar.** Removed per-row tags, kind chips, dividers, and the Helped button from the Notebook UI. Notes group cleanly under their parent category, and Curator chats group by date (Today, Yesterday, Previous 7 days) with a floating orb intro and a top-right orb toggle.
- **Cleaner Feed & consistent carousels.** Feed post headers now read `Project · icon · Agent · time` without branch pills or inline reply threads. Multi-image carousels lock to a consistent 16:10 frame with centered chevron controls, reliable slide loading, and click handling that doesn't accidentally open the lightbox.
- **OpenBot icon & Rune UI update.** Updates posted by OpenBot agents now show OpenBot's app icon instead of a generic initial, and the app is updated to the latest Rune UI.

## 0.5.1

- **Quieter callouts in the Notebook.** Notes, tips and warnings now sit in a plain outlined box with a small icon, instead of a thick coloured stripe down the side.

## 0.5.0

- **Multi-image & video carousels on updates.** Agents, the CLI (`feeder post -s ... -s ...`), and MCP (`post_update` with `screenshots`) can now attach up to 8 screenshots or clips to a single update. Update cards render an interactive carousel with arrows, a `1 / N` counter, and slide dots; Lightbox adds a bottom thumbnail filmstrip and compares slides within the same update first; Gallery displays every slide across updates; and `feeder serve` supports interactive carousels too.
- **Freeform Markdown notes in the Knowledge Library.** Creating a new note now opens directly onto a clean, borderless Notion/Obsidian-style Markdown canvas — just a large title, a single inline category picker, a slim formatting toolbar, live inline Markdown styling, smart list continuation, and a Write / Preview toggle. Existing structured pages migrate automatically into clean Markdown.
- **Rich Markdown reading with callouts & interactive checklists.** Notebook articles now render full GFM Markdown, syntax-highlighted code blocks, tables, Obsidian callouts (`> [!note]`, `[!tip]`, `[!warning]`, `[!important]`, `[!caution]`), and clickable task checkboxes that save straight back to SQLite.

## 0.4.2

- **Cleaner Needs you handling.** Open decision or blocker cards now include a one-click Dismiss button with Undo, answered cards show a quiet Answered pill instead of staying amber, and bare `Decision:` lines no longer cause false alarms. You can also clear flags with `feeder dismiss` or the new `dismiss_attention` MCP tool.
- **Scoped search & accurate Unread filter.** Pressing ⌘F inside a project filters that project's updates inline instead of switching tabs, and the Feed's Unread filter only shows updates newer than when you last opened each project.
- **Copy replies & safer drafts.** Human replies have a one-click Copy button, and reply, edit, or brief drafts are preserved if saving fails.
- **Deeper links & Notebook MCP resources.** Clicking notifications opens the exact update card (`feeder://project/<slug>/update/<id>`), `feeder://settings` opens Settings, and MCP clients can read `feeder://notebook` and `feeder://notebook/{id}` directly.
- **Menu bar & performance polish.** The menu bar companion now shares the exact Needs you rules with the app and polls SQLite off the main thread; the MCP server reuses its database connection; and the app skips reloading the Notebook on feed-only updates.
- **Local security hardening.** Secret scanning now protects updates, replies, and project briefs; data directories and config backups use restricted permissions; media imports reject symlinks and check file headers; and `feeder serve` enforces Host, Origin, size, and CSP guards.

## 0.4.1

- **A cleaner project page.** The header holds To-Do, Copy Context, Rename and Archive. Show in Finder and Pin are on the project's orb menu on Home, and Copy Context does what Copy Prompt did.
- **A simpler Notebook heading.** The page count and the line under the heading are gone.
- **Matching corners.** The Notebook's main pane now curves in line with the window's corner.
- **Buttons and menu items in Title Case**, such as Copy Context and Mark All Read.

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
