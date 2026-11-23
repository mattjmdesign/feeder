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

pub const NOTEBOOK_GUIDE: &str = r#"# Feeder's notebook: a shared Markdown knowledge library across projects

The notebook is one shared, searchable library of freeform Markdown articles across every project
and every agent: non-obvious bug fixes, approaches that failed, reusable patterns, architecture
decisions, and technical notes. Every project and every agent reads the same library, so what you
learn once pays off everywhere.

## Look before you dig
When starting or resuming work, pass a few task keywords to `get_feed`, for example
`{"project":"Orbit","task":"appcast signing"}`. It includes up to three relevant current
notes from that project and the shared Notebook. Open the ones you need with `read_page`.

Search the notebook before non-trivial debugging, before picking an approach for something tricky,
and whenever an error message isn't immediately familiar:

    feeder notebook search "sqlite busy timeout"        (MCP: search_notebook)

Results show each article's title and excerpt. Open one in full with `feeder notebook show 12`
(MCP: `read_page`). If an article helped, record it with `feeder notebook helped 12`
(MCP: `mark_page_helped`) so the most useful notes rise in search.

## Flag gaps for Curator
Use `flag_knowledge` when a reusable lesson is missing, wrong, or outdated. Include the affected
`page_id` if a note exists. Put the verified fix in `suggestion` when you have one.

    feeder flag --kind outdated --page 12 --suggest 'Run scripts/release.sh' 'The release script moved out of the repository root.'

Flag non-obvious fixes, pitfalls, reusable procedures, and decisions that will help the next agent.
Keep status updates in the Feed. Leave out secrets, tokens, keys, and customer data.

Curator reviews flags and maintains the shared notes. As a working agent, never create, edit,
archive, or restore shared notes. Flag the change and keep working.
"#;

pub fn notebook_skill_markdown() -> String {
    format!(
        "---\nname: feeder-notebook\ndescription: Search Feeder's shared Notebook before non-trivial debugging or choosing an unfamiliar approach. Flag missing, wrong, or outdated knowledge for Curator, with a verified suggestion when available.\n---\n\n{NOTEBOOK_GUIDE}"
    )
}

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
    fn the_notebook_skill_teaches_search_and_flagging() {
        let skill = super::notebook_skill_markdown();
        assert!(skill.starts_with("---\nname: feeder-notebook\n"));
        assert!(skill.contains("## Look before you dig"));
        assert!(skill.contains("`flag_knowledge`"));
        assert!(skill.contains("`suggestion`"));
        assert!(!skill.contains("write_page"));
        assert!(!skill.contains("feeder notebook write"));
    }
}
