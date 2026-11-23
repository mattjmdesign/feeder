pub const MAX_PARAGRAPHS: usize = 3;
pub const MAX_CHARS: usize = 900;

#[derive(Clone, Copy)]
struct Fence {
    marker: char,
    width: usize,
}

impl Fence {
    fn advance(active: &mut Option<Self>, line: &str) -> bool {
        let trimmed = line.trim_start_matches(' ');
        let marker = trimmed.chars().next();
        let candidate = marker.filter(|c| matches!(c, '`' | '~')).filter(|_| line.len() - trimmed.len() <= 3);
        let was_active = active.is_some();
        if let Some(marker) = candidate {
            let width = trimmed.chars().take_while(|&c| c == marker).count();
            let rest = &trimmed[width..];
            match *active {
                Some(open) if marker == open.marker && width >= open.width && rest.trim().is_empty() => *active = None,
                None if width >= 3 && (marker == '~' || !rest.contains('`')) => *active = Some(Self { marker, width }),
                _ => {}
            }
        }
        was_active || active.is_some()
    }
}

pub fn update_blocks(mut rest: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        while let Some(line) = rest.split_inclusive('\n').next().filter(|line| line.trim().is_empty()) {
            rest = &rest[line.len()..];
        }
        if rest.is_empty() {
            return None;
        }
        let mut fence = None;
        let end =
            rest.split_inclusive('\n').take_while(|line| Fence::advance(&mut fence, line) || !line.trim().is_empty()).map(str::len).sum();
        let (block, tail) = rest.split_at(end);
        rest = tail;
        Some(block.trim_end_matches(['\n', '\r']))
    })
}

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

impl std::error::Error for UpdateError {}

pub fn validate_body(raw: &str) -> Result<String, UpdateError> {
    let normalized = raw.replace("\r\n", "\n");
    let paragraphs: Vec<&str> = update_blocks(&normalized).map(str::trim).collect();
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

pub fn parse_attention(body: &str) -> Option<AttentionKind> {
    let mut fence = None;
    for line in body.lines().filter(|line| !Fence::advance(&mut fence, line)) {
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
    None
}

pub fn extract_block(body: &str, prefixes: &[&str]) -> Option<String> {
    for block in update_blocks(body) {
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
    fn fenced_code_keeps_blank_lines_and_does_not_raise_attention() {
        let body = "Done: verified the command.\n~~~text\necho first\n\nBlocked: example output\n~~~\n\nWhy: the sample needs both lines.\n\nNext: publish the docs.";
        assert_eq!(validate_body(body).expect("three update blocks"), body);
        assert_eq!(parse_attention(body), None);
        assert_eq!(extract_block(body, &["next"]).as_deref(), Some("publish the docs."));
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
}
