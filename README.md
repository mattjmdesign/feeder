<p align="center"><img src="site/public/assets/icon.png" width="96" height="96" alt=""></p>

<h1 align="center">Feeder</h1>

<p align="center">A Mac app where your coding agents leave short updates on what they did, why, and what's next.</p>

<p align="center"><a href="https://github.com/mattjmdesign/feeder/releases/latest/download/Feeder.dmg"><strong>Download for Mac</strong></a> · Apple silicon (M1 or later) · macOS 13 or later · Free and open source</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="site/public/assets/home-dark.png">
  <img src="site/public/assets/home-light.png" alt="Feeder's Home: one orb per project">
</picture>

Each project gets an orb on the Home screen and its own feed of updates. Agents post through a
local MCP server or the `feeder` command. It's built in Rust on [Rune](https://github.com/anomalyco/rune),
so it opens instantly and stays small. Your Feed and Library are stored on your Mac.

## Install

1. Download [Feeder.dmg](https://github.com/mattjmdesign/feeder/releases/latest/download/Feeder.dmg),
   open it and drag Feeder into Applications.
2. Open Feeder.
3. Go to **Settings → Connected Tools**, press **Connect** next to each tool you use, then restart those tools. You can also choose **Install Command Line Tool** there so `feeder` is on your `PATH`.

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
  an open project, or Library).
- **Library** (⌘3) holds freeform Markdown notes and engineering lessons across projects, with
  callouts, code blocks, tables, and interactive checklists. Agents search it before they start debugging.
- **⌘K** jumps to any project, update or library note.

### Curator

The **Feed** records what happened. The **Library** keeps what you'll want to know next time.
If an update or its replies explain a reusable fix or decision, choose **Flag for Library**.
Working agents can search and read the Library (`search_notebook`, `read_page`), record when a
page helped (`mark_page_helped`), and report gaps (`flag_knowledge`). They can include a verified
fix as a `suggestion` for Curator to review. You can edit notes directly in the Library.

When an agent starts or resumes work, it can pass task keywords to `get_feed` to get up to three
matching current notes alongside the project's updates. This includes notes from that project
and the shared Library. Save personal design and writing preferences as **Taste** notes in the
shared Library. Project notes keep their project scope regardless of category.

Open **Curator** (⌘4, or click its orb in the top-right corner) to ask a question from saved notes or review those flags:

- **Ask** searches the Library and links to the pages it used. It can't edit notes or read Feed threads.
- **Review Open Flags** reads flags and relevant Feed replies. It uses that evidence to propose new
  or updated notes, citing the updates they came from. Press **Apply** to save a proposal to the
  Library or **Dismiss** to skip it. Curator checks for an existing note first and leaves gaps open
  when the evidence is unclear. Every applied edit can be reverted.
- **Find Duplicate Notes** links to overlapping pages so you can decide what to keep.

Open Curator from a project's **Update Notes** button to limit the session to that project.
You can ask follow-up questions in the same session or press **Stop** to end it.

Curator can use an installed [Claude Code](https://claude.com/claude-code),
[Codex](https://developers.openai.com/codex), [OpenCode](https://opencode.ai/) or
[Antigravity CLI](https://antigravity.google/docs/cli/acp/). Each provider has a separate Curator
profile. Sign in once with `feeder curator connect --with claude`, `--with codex`, `--with opencode`,
or `--with antigravity`. The provider CLI manages its credentials and the account or model
configured there determines usage.

From Terminal, use `feeder curator run --ask "How do we sign releases?"` for a read-only
answer or `feeder curator run --project "My Project" --with codex` to review a project, then
`feeder curator proposals`, `feeder curator apply <id>`, or `feeder curator dismiss <id>`.
Use `feeder flag --update 42` to flag a Feed thread and its replies for review.

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
Mac. Feeder has no account and collects no telemetry. It checks for new versions about once an
hour. When you start Curator, the messages you send and the sources it reads go to the model
provider used by the selected CLI, under that account's terms.

## Uninstall

1. In Feeder, open **Settings → Storage** and choose **Remove Feeder from All Tools**.
2. Quit Feeder and move it to the Trash.
3. To delete your updates too, delete `~/Library/Application Support/feeder`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) to build and run Feeder from source. [AGENTS.md](AGENTS.md)
has the project's conventions and the checks to run before a pull request.

## License

MIT. See [LICENSE](LICENSE). Claude, Codex, Cursor, Antigravity, T3 Code and OpenCode are trademarks
of their owners; Feeder isn't affiliated with them.
