#[derive(Debug, PartialEq, Eq)]
pub struct Section {
    pub label: Option<String>,
    pub markdown: String,
}

pub fn sections(body: &str) -> Vec<Section> {
    feeder_core::update_blocks(body)
        .map(str::trim)
        .map(|p| {
            let (label, rest) = split_label(p);
            let rest = if label.is_some() { capitalize(rest) } else { rest.to_string() };
            let markdown = if rest.contains(['\n', '`', '[', '*', '_', '<', '>', '~', '\\', '|']) || rest.starts_with('#') {
                rest
            } else {
                enrich(&code_tickets(&rest))
            };
            Section { label, markdown }
        })
        .collect()
}

const LABELS: [&str; 26] = [
    "done",
    "changed",
    "shipped",
    "fixed",
    "added",
    "updated",
    "removed",
    "why",
    "benefit",
    "impact",
    "found",
    "blocked",
    "next",
    "up next",
    "status",
    "where it stands",
    "left off",
    "decision",
    "decision needed",
    "testing",
    "tests",
    "summary",
    "notes",
    "note",
    "context",
    "risk",
];

fn split_label(p: &str) -> (Option<String>, &str) {
    let (bold, s) = match p.strip_prefix("**") {
        Some(rest) => (true, rest),
        None => (false, p),
    };
    let Some(colon) = s.find(':') else { return (None, p) };
    let label = &s[..colon];
    let mut after = &s[colon + 1..];
    if bold {
        let Some(rest) = after.strip_prefix("**") else { return (None, p) };
        after = rest;
    }
    let trimmed = label.trim();
    let words = trimmed.split_whitespace().count();
    let plain = trimmed.chars().all(|c| c.is_alphanumeric() || " -'&/".contains(c));
    let rest = after.trim_start();
    if trimmed.is_empty() || trimmed.chars().count() > 32 || words == 0 || words > 4 || !plain || rest.is_empty() {
        return (None, p);
    }
    if !(after.starts_with(' ') || after.starts_with('\n')) {
        return (None, p);
    }
    if !bold && !LABELS.contains(&trimmed.to_ascii_lowercase().as_str()) {
        return (None, p);
    }
    (Some(trimmed.to_string()), rest)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_lowercase() => c.to_uppercase().chain(chars).collect(),
        _ => s.to_string(),
    }
}

fn code_tickets(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut in_code = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            in_code = !in_code;
            out.push(c);
            i += 1;
            continue;
        }
        let boundary = i == 0 || !chars[i - 1].is_alphanumeric();
        if !in_code && boundary && c.is_ascii_uppercase() {
            let caps = chars[i..].iter().take_while(|c| c.is_ascii_uppercase()).count();
            let dash = i + caps;
            let digits = chars.get(dash + 1..).map_or(0, |r| r.iter().take_while(|c| c.is_ascii_digit()).count());
            let end = dash + 1 + digits;
            let ends_clean = chars.get(end).is_none_or(|c| !c.is_alphanumeric());
            if (2..=8).contains(&caps) && chars.get(dash) == Some(&'-') && digits > 0 && ends_clean {
                out.push('`');
                out.extend(&chars[i..end]);
                out.push('`');
                i = end;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

const UNITS: [&str; 12] = ["ms", "s", "sec", "min", "kb", "mb", "gb", "fps", "hz", "px", "pt", "x"];
const CUES: [&str; 9] = ["Next:", "Blocked:", "Risk:", "Note:", "Todo:", "TODO:", "Warning:", "Heads-up:", "Decision:"];
const FILE_TYPES: [&str; 26] = [
    "rs", "ts", "tsx", "js", "jsx", "mjs", "json", "md", "mdx", "toml", "yml", "yaml", "py", "swift", "kt", "go", "css", "scss", "html",
    "sql", "sh", "app", "png", "svg", "lock", "env",
];

fn enrich(md: &str) -> String {
    let lead = lead_sentence(md);
    let mut out = String::with_capacity(md.len() + 32);
    for (i, code_part) in md.split('`').enumerate() {
        if i > 0 {
            out.push('`');
        }
        if i % 2 == 1 {
            out.push_str(code_part);
            continue;
        }
        for (j, bold_part) in code_part.split("**").enumerate() {
            if j > 0 {
                out.push_str("**");
            }
            out.push_str(&if j % 2 == 1 { bold_part.to_string() } else { enrich_words(bold_part) });
        }
    }
    match lead {
        Some(n) if !md.contains("**") => {
            let first: String = md.chars().take(n).collect();
            let enriched_first = enrich(&first);
            match out.strip_prefix(&enriched_first) {
                Some(rest) => format!("**{}**{rest}", enriched_first.trim_end()),
                None => out,
            }
        }
        _ => out,
    }
}

fn lead_sentence(md: &str) -> Option<usize> {
    if md.starts_with(['-', '*', '1']) || md.contains('\n') {
        return None;
    }
    let chars: Vec<char> = md.chars().collect();
    let end =
        (1..chars.len().saturating_sub(2)).find(|&i| ".!?".contains(chars[i]) && chars[i + 1] == ' ' && chars[i + 2].is_uppercase())?;
    let len = end + 1;
    (12..=110).contains(&len).then_some(len)
}

fn enrich_words(text: &str) -> String {
    let words: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        if let Some(next) = words.get(i + 1) {
            let unit = next.trim_end_matches([',', '.', ';', ')']).to_ascii_lowercase();
            if is_number(w) && UNITS.contains(&unit.as_str()) {
                let tail = &next[unit.len()..];
                out.push(format!("**{w} {}**{tail}", &next[..unit.len()]));
                i += 2;
                continue;
            }
        }
        out.push(enrich_word(w));
        i += 1;
    }
    out.join(" ")
}

fn enrich_word(w: &str) -> String {
    if CUES.contains(&w) {
        return format!("**{w}**");
    }
    let start = w.find(|c: char| !"(\"'".contains(c)).unwrap_or(w.len());
    let core_end = w.trim_end_matches([',', '.', ';', ':', ')', '"', '\'', '!', '?']).len().max(start);
    let (pre, core, post) = (&w[..start], &w[start..core_end], &w[core_end..]);
    if core.is_empty() || core.contains("](") || core.contains("://") {
        return w.to_string();
    }
    if is_ratio(core) || is_percent(core) {
        return format!("{pre}**{core}**{post}");
    }
    if is_hash(core) || is_pathlike(core) || is_file(core) {
        return format!("{pre}`{core}`{post}");
    }
    w.to_string()
}

fn is_number(w: &str) -> bool {
    !w.is_empty() && w.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',') && w.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn is_ratio(w: &str) -> bool {
    w.split_once('/').is_some_and(|(a, b)| is_number(a) && is_number(b) && !b.contains('/'))
}

fn is_percent(w: &str) -> bool {
    w.strip_suffix('%').is_some_and(is_number)
}

fn is_hash(w: &str) -> bool {
    (7..=40).contains(&w.len())
        && w.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        && w.chars().any(|c| c.is_ascii_digit())
        && w.chars().any(|c| c.is_ascii_alphabetic())
}

fn is_pathlike(w: &str) -> bool {
    if !w.contains('/') || w.starts_with('/') && w.len() < 3 || is_ratio(w) {
        return false;
    }
    let parts: Vec<&str> = w.split('/').collect();
    let ok_chars = w.chars().all(|c| c.is_alphanumeric() || "/-_.~@".contains(c));
    let shaped = w.starts_with('~') || w.starts_with("./") || parts.len() >= 3 || w.contains(['-', '_', '.']);
    ok_chars && shaped && parts.iter().filter(|p| !p.is_empty()).count() >= 2
}

fn is_file(w: &str) -> bool {
    let Some((stem, ext)) = w.rsplit_once('.') else { return false };
    !stem.is_empty()
        && stem.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c))
        && FILE_TYPES.contains(&ext)
        && !stem.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_labels_become_headings_and_sentences_stay() {
        let s = sections(
            "Done: moved parsing to a worker.\n\nWhy: keeps scrolling smooth under heavy feeds.\n\nNext: wire search retries.\n\n**Where it stands:** clean, ready for review.",
        );
        assert_eq!(s[0].label.as_deref(), Some("Done"));
        assert_eq!(s[0].markdown, "Moved parsing to a worker.");
        assert_eq!(s[1].label.as_deref(), Some("Why"));
        assert_eq!(s[1].markdown, "Keeps scrolling smooth under heavy feeds.");
        assert_eq!(s[2].label.as_deref(), Some("Next"));
        assert_eq!(s[2].markdown, "Wire search retries.");
        assert_eq!(s[3].label.as_deref(), Some("Where it stands"));
        assert_eq!(s[3].markdown, "Clean, ready for review.");
        let plain = sections("Shipped the onboarding flow and fixed the bug in billing: it rounded down.");
        assert_eq!(plain[0].label, None);
        assert_eq!(sections("Meeting at 12:30 today.")[0].label, None);
    }

    #[test]
    fn authored_links_and_fenced_code_reach_the_renderer_intact() {
        let s =
            sections("Done: Read the [release notes](https://example.test/TAL-42).\n\n```text\nfirst line\n\nNext: example output\n```");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].markdown, "Read the [release notes](https://example.test/TAL-42).");
        assert_eq!(s[1].markdown, "```text\nfirst line\n\nNext: example output\n```");
    }

    #[test]
    fn results_branches_and_files_stand_out() {
        let e = enrich("Main is pushed at 520e7e5 on sam/wave-3-plan. Tests: 44/44 pass, 85% coverage, 14 ms frames.");
        assert!(e.contains("`520e7e5`") && e.contains("`sam/wave-3-plan`"), "{e}");
        assert!(e.contains("**44/44**") && e.contains("**85%**") && e.contains("**14 ms**"), "{e}");
        let e = enrich("Edited README.md and src/main.rs and/or the docs. Next: ship it.");
        assert!(e.contains("`README.md`") && e.contains("`src/main.rs`") && e.contains("and/or") && !e.contains("`and/or`"), "{e}");
        assert!(e.contains("**Next:**"), "{e}");
        assert_eq!(enrich("Keep `a/b-c` and **ship 3/4** as written"), "Keep `a/b-c` and **ship 3/4** as written");
    }

    #[test]
    fn a_short_opening_sentence_leads_in_bold_when_more_follows() {
        assert_eq!(
            enrich("Wave 3 planning is underway. The specs landed today."),
            "**Wave 3 planning is underway.** The specs landed today."
        );
        assert_eq!(enrich("Only one sentence here."), "Only one sentence here.");
        let long =
            "A very long first sentence that goes on and on about many things in great detail, far past what a lead should be. Then more.";
        assert!(!enrich(long).starts_with("**"));
    }

    #[test]
    fn ticket_ids_read_as_code_outside_code_spans() {
        assert_eq!(code_tickets("Fixed TAL-249 and TAL-248."), "Fixed `TAL-249` and `TAL-248`.");
        assert_eq!(code_tickets("see `TAL-1` and UTF-8 and X-1"), "see `TAL-1` and `UTF-8` and X-1");
        assert_eq!(code_tickets("ABCDEFGHI-2 aTAL-3"), "ABCDEFGHI-2 aTAL-3");
    }
}
