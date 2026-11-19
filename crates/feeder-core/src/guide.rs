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

## Media: attach one screenshot or short clip when the work is visible
- Attach **one** screenshot (`png`/`jpg`/`webp`/`gif`) or short clip (`mp4`/`mov`/`webm`) via `screenshot_path` / `--screenshot` whenever the change affects something visible: UI, layout, chart, motion, or rendered output.
- Use a real capture of the result (app window, browser, recording, or headless snap), cropped cleanly—never a mockup.
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

    Next: …" --screenshot ./shot.png

MCP: call `post_update` with `summary` (and `screenshot_path` when visible).
"#;

/// How agents should use the notebook: look before digging, write only lessons that will save
/// someone time, and keep it tidy. Served by the MCP server, `feeder notebook guide` and the
/// `feeder-notebook` skill.
pub const NOTEBOOK_GUIDE: &str = r#"# Feeder's notebook: lessons that carry across projects

The notebook is one shared, searchable record of what was learned while building: bugs and how
they were fixed, approaches that failed, patterns that work, and decisions with their reasons.
Every project and every agent reads the same notebook, so a lesson learned once pays off everywhere.

## Look before you dig
Search the notebook before non-trivial debugging, before picking an approach for something tricky,
and whenever an error message isn't immediately familiar:

    feeder notebook search "sqlite busy timeout"        (MCP: search_notebook)

Results show each page's title and rule. Open one with `feeder notebook show 12` (MCP: `read_page`).
If a page helped, record it with `feeder notebook helped 12` (MCP: `mark_page_helped`) so good
pages rise to the top.

## Write a page only when it will save someone time later
- **Fix**: a bug whose cause wasn't obvious (more than a typo), and how it was solved.
- **Pitfall**: an approach that failed or broke something, and why it would trap someone again.
- **Pattern**: an approach that worked notably well and applies beyond this one spot.
- **Decision**: a design or architecture choice that was settled, and the reasoning.

Don't write pages for one-off typos, project trivia (file paths, to-dos), what the docs already say,
or status (that goes in the feed). Most sessions write no page at all; that's expected.

## Shape of a page
- **Topic**: a high-level home for the page (`rust`, `react`, `nextjs`, `react-native`, `swift`, `typescript`, `agents`, `data`, `design`, `tooling`, or a short custom slug). If omitted, Feeder infers one from your tags.
- **Title**: the situation in plain words, as someone would search for it.
  "SQLite reads stall while a long write holds the lock", not "DB fix".
- **What happened**: the symptom or situation, with the exact error text if there was one.
- **Why**: the root cause or the reasoning (optional, but most good pages have one).
- **Do this**: the rule to follow next time, one or two imperative sentences.
- **Tags**: 1–6 specific lowercase sub-tags under that topic: `sqlite`, `locks`, `app-router`, `layout`, `worklets`, `mcp`.

Write it so it helps in a different project: name the technology, not your file names.
Never include secrets, tokens, keys or customer data; the notebook refuses them.

## Keep it tidy
- Search first. If a page covers the same lesson, revise it (`write_page` with its `id`, or
  `feeder notebook write --id 12 …`) instead of adding another. Feeder refuses near-duplicates.
- If a page turns out wrong, correct it. If it's outdated, archive it (`archive_page`).

## Writing one

    feeder notebook write --kind fix --topic data --title "SQLite reads stall while a long write holds the lock" --tags sqlite,rust --what "The app froze for seconds while the CLI posted an update." --why "The write transaction stayed open while a large screenshot was copied." --do "Copy files before opening the write transaction; keep transactions to the insert."

MCP: `write_page` with `kind`, optional `topic`, `title`, `tags`, `what_happened`, `why`, `do_this`.
"#;

/// The skill that teaches agents to search the notebook before digging and to write the lessons
/// worth keeping.
pub fn notebook_skill_markdown() -> String {
    format!(
        "---\nname: feeder-notebook\ndescription: Search and grow Feeder's shared notebook of engineering lessons (fixes, pitfalls, patterns, decisions) across all of the user's projects. Use before non-trivial debugging, when an error message is unfamiliar, before choosing an approach for something tricky, and after solving a non-obvious bug, hitting a dead end or settling a design decision worth remembering.\n---\n\n{NOTEBOOK_GUIDE}"
    )
}

/// The skill `feeder setup` installs for agents that load skills (Claude Code, Codex, OpenCode…).
pub fn skill_markdown() -> String {
    format!(
        "---\nname: feeder-update\ndescription: Post or read a project update in Feeder, the user's local project update centre. Use when finishing a meaningful piece of work, when the user asks for a status/handoff/Feeder update, or when resuming a project (read recent updates first). Format posts in three short blocks (Done, Why & benefit, Next & why) and attach one real screenshot when changes are visible.\n---\n\n{GUIDE}"
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
