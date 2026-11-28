# Changelog

What changed in each version of Feeder, newest first. Each section is copied from
`release-notes/<version>.md` by `scripts/changelog.sh`; edit those files, not this one.

## 0.5.16

- Feed now frames the stream with vertical side dividers, a live activity summary at the top, full-row hover highlights, and always-visible Copy, Reply, and menu actions.
- Curator's model picker switches to a compact logo chip in narrow windows so the send button stays inside the composer.
- In-app update errors now tell you clearly when Feeder was replaced on disk while running, and local installs wait for Feeder to quit before replacing the app bundle.

## 0.5.15

- Library cards get their full-height two-tone artwork back: denser dot patterns, Rust keeps its checker, and each category keeps one consistent design.
- Card layouts stay compact, so the grid still shows plenty of notes at once.

## 0.5.14

- Library cards give titles more room and show the takeaway when a note has one. Breadcrumbs fit alongside the tabs in smaller windows.
- Open an update directly from its Needs You badge on Home. In Feed, Reply is easier to spot.
- Project actions for renaming and archiving moved to the bottom-left menu. Copy Context stays in the toolbar.
- Setup keeps its buttons visible while the tool list scrolls and confirms your first update when it arrives.
- Curator opens with a simpler chat screen. The proposal card shows the edit, and replies focus on evidence and open questions.

## 0.5.13

- Feeder preserves your custom agent guides and existing command line tools. Uninstall removes the unedited guides and command line links Feeder installed.
- The optional web companion requires a token when listening outside localhost.
- Feeder explains what connecting a tool changes and what Curator sends to the model provider. The README covers backup and restore.
- The welcome screen and empty states make it easier to connect a tool and post your first update.
- Agents can catch up on unread replies and open tasks, search the Library, and update project briefs and checklists through fewer Feeder tools.

## 0.5.12

- **A welcome screen for new installs.** Home walks you through connecting your coding tools, copying a prompt for your first update, and turning on Curator.
- **Helpful empty screens.** The Feed, Library, and Curator now say what to do next instead of sitting blank.
- **Feeder finds more of your tools.** It now looks where your terminal looks, plus common install folders, so Claude Code, OpenCode and others installed through npm, Homebrew, or their own installers show up. Use **Scan Again** in Settings or on Home after installing one.
- **Curator settings list the tools Feeder couldn't find**, so you can tell what's missing.
- The sidebar is a shade darker than the page, in both light and dark mode.
- Library card previews skip headings again, so they start with the note's first sentence.

## 0.5.11

- Fixed Feeder showing "Application Not Responding" in the Dock on macOS.
- Library notes ask before discarding unsaved edits, offer Undo after archiving a note, and sync sidebar search and new notes automatically.
- Curator keeps your scroll position cleanly across chats, preserves multi-turn retries, shows diff previews and categories on proposals, and prevents duplicate Apply, Dismiss, or Revert taps while saving.
- Feed and Quick Jump show image load errors with Show in Finder, let you reply from the Feed tab or jump to matched updates, hide Flag on queued or reviewed posts, and sort Before/After comparisons chronologically.
- Settings adds Command line tool setup under Connected Tools, retries tool disconnects accurately, and suppresses notification banners when you are already viewing that project.
- `feeder curator` adds `proposals`, `apply`, and `dismiss` subcommands, and agents can flag or write shared Library notes via MCP and the CLI.

## 0.5.10

- Library cards are wider and give titles two lines. Cards stay the same size across categories.
- Curator asks for a fresh proposal if the note changed before you approve it.
- Links to older project updates scroll to the right post.
- The web companion keeps your reply draft and shows an error if sending fails.

## 0.5.9

- **OpenCode can use Feeder tools in Curator.** Review flagged lessons, read Feed updates, and propose Library notes from the installed Mac app.
- **Update checks show a fresh result.** If Sparkle is already checking in the background, Feeder no longer repeats an earlier “up to date” message.
- **Removed the macOS menu bar companion.** Open Feeder from the Dock or Applications.

## 0.5.8

- **Propose & Approve cards in Curator chat.** Every Curator chat can now inspect your entire Library, open flagged lessons, and recent Feed updates, replies, and project briefs, and propose creating, updating, or archiving Library notes right inside chat. Each proposal renders as an inline card with the reason, a unified diff preview, and **Apply** / **Dismiss** buttons (switching to **Open** and **Revert** once applied).
- **Fixed OpenCode Curator tool loading.** OpenCode Curator sessions now declare Feeder's scoped Curator MCP server directly in the session config and disable inherited global MCP servers, preventing collisions with a user's global `feeder mcp` entry that previously left Curator with zero Library tools.
- **Notebook is now Library.** Renamed Notebook to **Library** across the top tab (`Home`, `Feed`, `Library`), breadcrumbs, search bar, Quick Jump (`⌘K`), Feed flag links, and Settings, while keeping `feeder library` / `feeder notebook` and `feeder://library/...` / `feeder://notebook/...` compatible.
- **One-click Curator actions and deeper lesson drafts.** Added **Check Recent Feed Updates** alongside **Review Flagged Lessons** on Curator's home screen, and updated the MCP instructions and `flag_knowledge` schema so coding agents keep Feed updates short (`Done / Why / Next`) while submitting full Markdown lesson drafts in `suggestion` whenever they learn a reusable fix or receive a correction.

## 0.5.7

- **Curator as its own top-level view.** Clicking the Curator orb now opens Curator on its own screen instead of inside Notebook. `Home`, `Feed`, and `Notebook` clear their active highlight while Curator is open, and Curator gets its own breadcrumb (`Curator`), chat search, `+ New Chat` action, and date-grouped chat history.
- **Predictable top-bar navigation.** Clicking **Home**, **Feed**, or **Notebook** now always takes you to that section's home screen (closing any open project, note, editor, or category filter), and clicking Notebook never lands in Curator.
- **Curator shortcuts and quick jump.** Added `⌘4` to open Curator, **Go to Curator** in Quick Jump (`⌘K`), `⌘F` to search chats inside Curator, `⌘\` to toggle Curator's sidebar, and `feeder://curator` deep links.

## 0.5.6

- **Faster launch and smoother scrolling.** Updated Rune UI for parallel macOS startup work and persistent icon mask caching across launches. Project update feeds now virtualize rows lazily like the main Feed, and layout callbacks skip unnecessary redraws when the window size hasn't changed.
- **Responsive image copy and cleaner orb transitions.** Copying non-PNG screenshots from the lightbox now runs off the UI thread so the window stays responsive. Opening a pulsing project from Home no longer clips its glow into a square during the transition.
- **Polish and reliability fixes.** Matched heading weight in the live Markdown editor to the app's type scale, fixed Title Case on Restart to Update, removed panic-prone unwraps in the Files view, added defensive guards for macOS notifications, and updated the About window privacy copy to cover Curator.

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
