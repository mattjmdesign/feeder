//! How agents should write an update. One text, served everywhere an agent might look: the MCP
//! server's instructions and `update` prompt, `feeder guide`, `feeder post --help` and the skill
//! `feeder setup` installs.

pub const GUIDE: &str = r#"# Writing a Feeder update

Feeder is the user's local project update centre: one feed per project, plus one stream across all of them.
Each post gives the user and next agent a complete handoff in seconds.

## When to post
- After finishing a meaningful stretch of work: a feature, fix, investigation, or release.
- When the user asks for an update, or before handing a project off.
- Skip trivial edits; one clear update beats five noisy ones.
- Resuming a project? Read recent context first with `get_feed` (or `feeder feed -n 3`).

## Shape: 3 short blocks, 900 characters max
Structure every update around the same three parts (separated by blank lines) so projects scan uniformly:

1. **What was done** (`Done:`, `Shipped:`, `Fixed:`, or `Changed:`): the concrete outcome and verification proof, not a file dump.
2. **Why it was done & benefit** (`Why:`, `Benefit:`, or `Impact:`): the problem it solved or what it unlocks now—never a restatement of block 1.
3. **Recommended next step & why** (`Next:`, or `Blocked:` / `Decision needed:`): the highest-leverage next move and why it matters.

Example:

    Done: added FTS5 search (`⌘F`/`⌘K`), git branch pills, and zoomable screenshot previews (**38/38** tests pass).

    Why: substring filtering missed older cards and screenshots couldn't be inspected closely, making cross-project handoffs harder to verify.

    Next: exercise `feeder://` deep links and `Copy image` in daily workflows so any rough edge surfaces before sharing wider.

## Voice and formatting (Markdown renders)
- Lead with the user-visible outcome, not a file inventory or commit dump.
- Use active voice and past tense for completed work. Skip "I", "successfully", and filler openers like "Catch-up from git:".
- **Bold** sparingly for the key takeaway, blocker, or decision needed.
- `code` for files, branches, commands, commits, versions, and ticket IDs.
- Use concrete numbers (`44/44 tests pass`, `14 ms -> 3 ms`, `3 of 5 screens done`).
- Avoid `#` headings, tables, long code blocks, logs, secrets (updates, replies, and briefs refuse tokens/keys), or repeating the project name.

## Media: attach screenshots or clips when the work is visible
- Attach **one or more** screenshots (`png`/`jpg`/`webp`/`gif`) or short clips (`mp4`/`mov`/`webm`)—up to **8** per update—via `screenshots` (or `screenshot_path`) in MCP, or repeated `--screenshot` flags in CLI, whenever the change affects something visible: UI, layout, before/after, multiple screens, motion, or rendered output.
- Multiple attachments render as an interactive carousel on the update card and in the lightbox.
- Use real captures of the result (app window, browser, recording, or headless snap), cropped cleanly—never mockups.
- Skip media for pure backend, refactor, test, or config changes.

## Collaborating with the user
- Read recent updates, open project to-dos, and any unread human replies first with `get_feed` (or `feeder feed -n 3`).
- Use `Decision needed:` or `Blocked:` in block 3 whenever you want the user to answer or unblock something (bare `Decision:` does not flag **Needs you**); Feeder highlights open cards so the user can reply inline or dismiss them.
- If a flagged decision or blocker was resolved out-of-band without a new update, clear the badge with `dismiss_attention` (or `feeder dismiss`).
- When a project has open to-dos you completed (or want to track), check them off via `toggle_todo` (or `feeder todo done <id>`).
- Emit a brief `pulse` while working on a longer task so the project's orb shows live activity.

## Posting
Project, branch, commit SHA, diff stat, and agent are detected automatically from where you run; omit them unless asked.

CLI (from the project folder):

    feeder post "Done: …

    Why: …

    Next: …" --screenshot ./before.png --screenshot ./after.png

MCP: call `post_update` with `summary` (and `screenshots` or `screenshot_path` when visible).
"#;

/// How agents should use the notebook: look before digging, write clean Markdown articles worth
/// keeping, and keep it tidy. Served by the MCP server, `feeder notebook guide` and the
/// `feeder-notebook` skill.
pub const NOTEBOOK_GUIDE: &str = r###"# Feeder's notebook: a shared Markdown knowledge library across projects

The notebook is one shared, searchable library of freeform Markdown articles across every project
and every agent: non-obvious bug fixes, approaches that failed, reusable patterns, architecture
decisions, and technical notes. Every project and every agent reads the same library, so what you
learn once pays off everywhere.

## Look before you dig
Search the notebook before non-trivial debugging, before picking an approach for something tricky,
and whenever an error message isn't immediately familiar:

    feeder notebook search "sqlite busy timeout"        (MCP: search_notebook)

Results show each article's title and excerpt. Open one in full with `feeder notebook show 12`
(MCP: `read_page`). If an article helped, record it with `feeder notebook helped 12`
(MCP: `mark_page_helped`) so the most useful notes rise in search.

## Write a note only when it will save someone time later
Good notes capture things worth carrying into the next project:
- A non-obvious bug fix (symptom, exact error, root cause, fix)
- A pitfall or dead end worth avoiding next time
- A reusable pattern, recipe, or architecture decision with its reasoning

Skip one-off typos, project-local trivia, or status updates (those belong in the feed). Most
sessions write no note at all; that's expected.

## Shape of a note: clean, freeform Markdown
Write natural, well-structured `.md` just like Obsidian or Notion—no rigid form fields required:
- **Title**: plain, searchable phrasing (e.g. "SQLite reads stall while a long write holds the lock").
- **Category (`topic`)**: optional high-level home (`rust`, `react`, `nextjs`, `react-native`, `swift`, `typescript`, `agents`, `data`, `design`, `tooling`, `general`, or a short custom slug). Inferred automatically when omitted.
- **Body (`body`)**: freeform Markdown—use `##` headings, fenced code blocks with language tags, bullet lists, `- [ ]` checklists, tables, links, and `> [!note]` / `> [!tip]` / `> [!warning]` callouts.
- **Tags & kind**: optional (`tags`, `kind`: `note`, `fix`, `pitfall`, `pattern`, `decision`).

Write so it helps in a different project: name the technology and exact error, not local scratch paths.
Never include secrets, tokens, keys, or customer data; the notebook refuses them.

## Keep it tidy
- Search first. If an article already covers the topic, revise it (`write_page` with its `id`, or
  `feeder notebook write --id 12 …`) instead of creating a duplicate. Feeder refuses near-duplicates.
- If a note turns out wrong, update it. If it's outdated, archive it (`archive_page`).

## Writing one

    feeder notebook write --topic data --title "SQLite reads stall while a long write holds the lock" --body "## Symptom\n\nApp reads froze during media copy.\n\n## Rule\n\nCopy files to disk before opening a SQLite write transaction."

MCP: `write_page` with `title`, `body` (Markdown), and optional `topic`, `tags`, `kind`.
"###;

/// The skill that teaches agents to search the notebook before digging and to write the lessons
/// worth keeping.
pub fn notebook_skill_markdown() -> String {
    format!(
        "---\nname: feeder-notebook\ndescription: Search and grow Feeder's shared Markdown knowledge library (fixes, pitfalls, patterns, decisions, technical notes) across all of the user's projects. Use before non-trivial debugging, when an error message is unfamiliar, before choosing an approach for something tricky, and after solving a non-obvious bug, hitting a dead end or settling a design decision worth remembering.\n---\n\n{NOTEBOOK_GUIDE}"
    )
}

/// The skill `feeder setup` installs for agents that load skills (Claude Code, Codex, OpenCode…).
pub fn skill_markdown() -> String {
    format!(
        "---\nname: feeder-update\ndescription: Post or read a project update in Feeder, the user's local project update centre. Use when finishing a meaningful piece of work, when the user asks for a status/handoff/Feeder update, or when resuming a project (read recent updates first). Format posts in three short blocks (Done, Why & benefit, Next & why) and attach one or more real screenshots/clips when changes are visible.\n---\n\n{GUIDE}"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_skill_has_front_matter_and_the_guide() {
        let skill = super::skill_markdown();
        assert!(skill.starts_with("---\nname: feeder-update\n"));
        assert!(skill.contains("Done:"));
        assert!(skill.contains("Why:"));
        assert!(skill.contains("Next:"));
        assert!(skill.contains("## Media"));
    }

    #[test]
    fn the_notebook_skill_teaches_search_first() {
        let skill = super::notebook_skill_markdown();
        assert!(skill.starts_with("---\nname: feeder-notebook\n"));
        assert!(skill.contains("## Look before you dig"));
        assert!(skill.contains("write_page"));
    }
}
