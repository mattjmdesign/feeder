//! What an update may be: 1–3 short blocks, capped so updates stay scannable.

/// Blocks (paragraphs or short lists) are separated by a blank line.
pub const MAX_PARAGRAPHS: usize = 3;
/// A ceiling on the whole note, so updates stay a glance rather than an essay.
pub const MAX_CHARS: usize = 900;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    Empty,
    TooManyParagraphs(usize),
    TooLong(usize),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Empty => f.write_str("update is empty; write 1–3 short blocks on what was done, why, and what's next"),
            UpdateError::TooManyParagraphs(n) => {
                write!(f, "update has {n} blocks; keep it to {MAX_PARAGRAPHS} short blocks separated by blank lines")
            }
            UpdateError::TooLong(n) => write!(f, "update is {n} characters; trim it to {MAX_CHARS} or fewer"),
        }
    }
}

/// Trims the note, collapses runs of blank lines into one paragraph break and checks the limits.
/// Returns the note as it will be stored.
pub fn validate_body(raw: &str) -> Result<String, UpdateError> {
    let paragraphs: Vec<String> = raw
        .replace("\r\n", "\n")
        .split("\n\n")
        .map(|p| p.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if paragraphs.is_empty() {
        return Err(UpdateError::Empty);
    }
    if paragraphs.len() > MAX_PARAGRAPHS {
        return Err(UpdateError::TooManyParagraphs(paragraphs.len()));
    }
    let body = paragraphs.join("\n\n");
    let chars = body.chars().count();
    if chars > MAX_CHARS {
        return Err(UpdateError::TooLong(chars));
    }
    Ok(body)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionKind {
    DecisionNeeded,
    Blocked,
}

/// Detects whether an update explicitly flags a blocker or a decision needed from the human.
pub fn parse_attention(body: &str) -> Option<AttentionKind> {
    for block in body.split("\n\n") {
        for line in block.lines() {
            let trimmed = line.trim().trim_start_matches(['-', '*', ' ']);
            let clean = trimmed.trim_start_matches("**");
            let lower = clean.to_ascii_lowercase();
            if lower.starts_with("decision needed:") || lower.starts_with("decision needed**") {
                return Some(AttentionKind::DecisionNeeded);
            }
            if lower.starts_with("blocked:") || lower.starts_with("blocked**") {
                return Some(AttentionKind::Blocked);
            }
        }
    }
    None
}

/// Extracts the text of the first paragraph matching one of `prefixes` (e.g. `"done"`, `"next"`).
pub fn extract_block(body: &str, prefixes: &[&str]) -> Option<String> {
    for block in body.split("\n\n") {
        let t = block.trim();
        let no_bold = t.strip_prefix("**").unwrap_or(t);
        let Some(colon) = no_bold.find(':') else { continue };
        let label = no_bold[..colon].trim().trim_end_matches("**").to_ascii_lowercase();
        if prefixes.iter().any(|p| p.eq_ignore_ascii_case(&label)) {
            let rest = no_bold[colon + 1..].trim_start_matches("**").trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// Generates a concise handoff prompt to paste into any coding agent to resume work on a project.
pub fn build_resume_prompt(
    project: &crate::Project,
    latest: Option<&crate::FeedUpdate>,
    replies: &[crate::Reply],
    todos: &[crate::TodoItem],
    now: i64,
) -> String {
    let mut lines = Vec::new();
    let branch_hint = latest.and_then(|u| u.branch.as_deref()).map(|b| format!(" on branch `{b}`")).unwrap_or_default();
    let path_hint = project.path.as_ref().map(|p| format!(" ({})", p.display())).unwrap_or_default();
    lines.push(format!("Resume **{}**{branch_hint}{path_hint}.", project.name));
    if let Some(brief) = project.brief.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        lines.push(format!("Pinned brief:\n{brief}"));
    }
    let open_todos: Vec<&crate::TodoItem> = todos.iter().filter(|t| !t.done).take(8).collect();
    if !open_todos.is_empty() {
        let mut t_lines = vec!["Open to-dos:".to_string()];
        for t in open_todos {
            t_lines.push(format!("- [ ] {}", t.title));
        }
        lines.push(t_lines.join("\n"));
    }
    if let Some(u) = latest {
        lines.push(format!("Latest update ({} by {}):\n{}", crate::ago(u.created_at, now), u.agent, u.body));
    }
    if !replies.is_empty() {
        let mut r_lines = vec!["Recent replies:".to_string()];
        for r in replies.iter().take(4) {
            r_lines.push(format!("- {}: {}", r.author, r.body));
        }
        lines.push(r_lines.join("\n"));
    }
    lines.push(format!(
        "First call Feeder's `get_feed` (or read `feeder://project/{}/feed`) to verify full context before making edits.",
        project.slug
    ));
    lines.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_blocks_pass_and_blank_runs_collapse() {
        let body = validate_body("  First line.\nSame paragraph.  \n\n\n\n- Second.\n\nThird.\n").unwrap();
        assert_eq!(body, "First line.\nSame paragraph.\n\n- Second.\n\nThird.");
    }

    #[test]
    fn a_fourth_block_is_refused() {
        assert_eq!(validate_body("a\n\nb\n\nc\n\nd"), Err(UpdateError::TooManyParagraphs(4)));
    }

    #[test]
    fn empty_and_overlong_notes_are_refused() {
        assert_eq!(validate_body(" \n\n "), Err(UpdateError::Empty));
        assert!(matches!(validate_body(&"x".repeat(MAX_CHARS + 1)), Err(UpdateError::TooLong(_))));
    }

    #[test]
    fn attention_and_block_extraction_work() {
        let body = "Done: wired webhook verification.\n\nWhy: prevents silent drops.\n\nDecision needed: keep legacy token or rotate now?";
        assert_eq!(parse_attention(body), Some(AttentionKind::DecisionNeeded));
        assert_eq!(extract_block(body, &["done"]).as_deref(), Some("wired webhook verification."));
        assert_eq!(extract_block(body, &["next", "decision needed"]).as_deref(), Some("keep legacy token or rotate now?"));
        let blocked = "Done: ran integration suite.\n\n**Blocked:** waiting on staging DB credentials.";
        assert_eq!(parse_attention(blocked), Some(AttentionKind::Blocked));
        assert_eq!(parse_attention("Done: all green.\n\nNext: deploy."), None);
        assert_eq!(parse_attention("Done: shipped.\n\nDecision: went with SQLite for local storage."), None);
    }

    #[test]
    fn build_resume_prompt_includes_brief_todos_update_and_replies() {
        let project = crate::Project {
            id: 1,
            slug: "feeder".into(),
            name: "Feeder".into(),
            path: Some(std::path::PathBuf::from("/tmp/feeder")),
            orb: crate::Orb::generate(&[]),
            brief: Some("Goal: local project updates".into()),
            archived: false,
            pinned: true,
            seen_at: Some(0),
            created_at: 100,
        };
        let update = crate::FeedUpdate {
            id: 10,
            project_id: 1,
            agent: "Codex".into(),
            body: "Done: added schema.\n\nNext: wire CLI.".into(),
            screenshot: None,
            media_w: None,
            media_h: None,
            branch: Some("feat/todos".into()),
            created_at: 1_000,
            diff_stat: None,
            commit_sha: None,
            attention_dismissed_at: None,
        };
        let replies = vec![crate::Reply {
            id: 2,
            update_id: 10,
            project_id: 1,
            author: "You".into(),
            from_human: true,
            body: "Keep it optional.".into(),
            created_at: 2_000,
            read_at: None,
        }];
        let todos = vec![
            crate::TodoItem {
                id: 1,
                project_id: 1,
                title: "Already finished".into(),
                done: true,
                position: 1,
                created_at: 500,
                completed_at: Some(900),
            },
            crate::TodoItem {
                id: 2,
                project_id: 1,
                title: "Wire CLI command".into(),
                done: false,
                position: 2,
                created_at: 600,
                completed_at: None,
            },
        ];

        let prompt = build_resume_prompt(&project, Some(&update), &replies, &todos, 5_000);
        assert!(prompt.contains("Resume **Feeder** on branch `feat/todos` (/tmp/feeder)."));
        assert!(prompt.contains("Pinned brief:\nGoal: local project updates"));
        assert!(prompt.contains("Open to-dos:\n- [ ] Wire CLI command"));
        assert!(!prompt.contains("Already finished"));
        assert!(prompt.contains("Latest update"));
        assert!(prompt.contains("Recent replies:\n- You: Keep it optional."));
        assert!(prompt.contains("feeder://project/feeder/feed"));
    }
}
