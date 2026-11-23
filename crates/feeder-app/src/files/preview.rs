use std::borrow::Cow;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use feeder_core::{FilePreview, read_file_preview};
use rune::tok::TextStyle;
use rune::{Direction, El, GrowthSpec, Image, ImageCache, ScrollSpec, Span, SurfaceFit, Tx, Waker, col, el, rich, row, text};
use rune_kit::syntax::{self, Paint, SyntaxTheme};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, scroll_bar};

use crate::State;
use crate::style::{self, Palette};

/// Line height and code typography for the file viewer.
const CODE_LINE_H: f32 = 20.0;
const CODE_FONT_SIZE: f32 = 12.5;
const GUTTER_FONT_SIZE: f32 = 11.5;
const MONO_ADVANCE: f32 = 7.6;
/// Bounds per-row text shaping even for minified single-line bundles.
const MAX_LINE_BYTES: usize = 4096;
/// Bounds wrapped-mode DOM size so multi-thousand-line files stay responsive when wrapped.
const WRAP_PAGE_LINES: usize = 500;

#[derive(Default)]
pub(super) struct Preview {
    worker: Option<Worker>,
    sequence: u64,
    selected: Option<Selected>,
    wrap: bool,
    copied_until: Option<(u64, i64)>,
}

struct Worker {
    requests: Sender<Request>,
    results: Receiver<(u64, io::Result<FilePreview>)>,
}

struct Request {
    sequence: u64,
    root: PathBuf,
    path: String,
}

struct Selected {
    path: String,
    content: Content,
}

enum Content {
    Loading,
    Text(CodeDoc),
    Image { image: Image, ready: bool },
    Notice(&'static str),
}

struct CodeDoc {
    source: String,
    lang: Option<&'static str>,
    lines: Vec<CodeLine>,
    line_count: usize,
    digits: usize,
    max_cols: usize,
    wrap_page: usize,
}

struct CodeLine {
    /// 1-based line number, or `None` on continuation chunks of an over-long minified line.
    number: Option<usize>,
    range: Range<usize>,
    highlights: Vec<(Range<usize>, Paint)>,
    key: String,
}

impl CodeDoc {
    fn from_source(path: &str, source: String) -> Self {
        let lang = syntax::lang_for_path(path);
        let raw_highlights = syntax::highlight(lang, &source);
        let mut lines = Vec::new();
        let mut line_count = 0usize;
        let mut max_cols = 0usize;
        let mut hl_idx = 0usize;

        let mut offset = 0usize;
        for raw in source.split_inclusive('\n') {
            line_count += 1;
            let trimmed = raw.strip_suffix('\n').unwrap_or(raw);
            let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
            let line_start = offset;
            let line_end = line_start + trimmed.len();
            offset += raw.len();

            if line_start == line_end {
                let row_idx = lines.len();
                lines.push(CodeLine {
                    number: Some(line_count),
                    range: line_start..line_end,
                    highlights: Vec::new(),
                    key: format!("code-ln-{row_idx}"),
                });
                continue;
            }

            let mut chunk_start = line_start;
            let mut first_chunk = true;
            while chunk_start < line_end {
                let chunk_end = if line_end - chunk_start <= MAX_LINE_BYTES {
                    line_end
                } else {
                    source.floor_char_boundary(chunk_start + MAX_LINE_BYTES).max(chunk_start + 1)
                };
                let chunk = &source[chunk_start..chunk_end];
                max_cols = max_cols.max(display_columns(chunk));

                while hl_idx < raw_highlights.len() && raw_highlights[hl_idx].0.end <= chunk_start {
                    hl_idx += 1;
                }
                let mut highlights = Vec::new();
                let mut j = hl_idx;
                while j < raw_highlights.len() && raw_highlights[j].0.start < chunk_end {
                    let (r, paint) = &raw_highlights[j];
                    let s = r.start.max(chunk_start) - chunk_start;
                    let e = r.end.min(chunk_end) - chunk_start;
                    if s < e {
                        highlights.push((s..e, *paint));
                    }
                    if r.end <= chunk_end {
                        j += 1;
                    } else {
                        break;
                    }
                }

                let row_idx = lines.len();
                lines.push(CodeLine {
                    number: first_chunk.then_some(line_count),
                    range: chunk_start..chunk_end,
                    highlights,
                    key: format!("code-ln-{row_idx}"),
                });
                first_chunk = false;
                chunk_start = chunk_end;
            }
        }

        let digits = decimal_digits(line_count).max(3);
        Self { source, lang, lines, line_count, digits, max_cols, wrap_page: 0 }
    }

    fn wrap_page_count(&self) -> usize {
        self.lines.len().div_ceil(WRAP_PAGE_LINES).max(1)
    }
}

impl Preview {
    pub(super) fn is_open(&self) -> bool {
        self.selected.is_some()
    }

    pub(super) fn close(&mut self) {
        self.selected = None;
    }

    pub(super) fn open(&mut self, root: PathBuf, path: &str, deleted: bool, wake: Option<Waker>) {
        self.sequence = self.sequence.wrapping_add(1);
        let content = if deleted {
            Content::Notice("This file was deleted.")
        } else {
            let requested = self.worker.get_or_insert_with(|| Worker::start(wake)).requests.send(Request {
                sequence: self.sequence,
                root,
                path: path.to_owned(),
            });
            if requested.is_ok() { Content::Loading } else { Content::Notice("Couldn’t start the preview.") }
        };
        self.selected = Some(Selected { path: path.to_owned(), content });
    }

    pub(super) fn poll(&mut self, images: Option<&ImageCache>) -> bool {
        let latest = self.worker.as_ref().and_then(|w| w.results.try_iter().find(|(seq, _)| *seq == self.sequence));
        let Some(selected) = &mut self.selected else { return false };
        if let Some((_, result)) = latest {
            selected.content = match result {
                Ok(FilePreview::Text(source)) => Content::Text(CodeDoc::from_source(&selected.path, source)),
                Ok(FilePreview::Image(bytes)) => {
                    let Some(images) = images else {
                        selected.content = Content::Notice("Couldn’t display this image.");
                        return true;
                    };
                    let image = images.load(bytes);
                    if rune::tooling::headless_run().is_some() {
                        let _ = image.wait();
                    }
                    let ready = image.is_ready();
                    Content::Image { image, ready }
                }
                Ok(FilePreview::Unsupported) => Content::Notice("This file type can’t be previewed."),
                Ok(FilePreview::TooLarge) => Content::Notice("This file is too large to preview."),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Content::Notice("This file is no longer in the project."),
                Err(_) => Content::Notice("Couldn’t open this file."),
            };
            return true;
        }
        if let Content::Image { image, ready } = &mut selected.content
            && !*ready
            && image.is_ready()
        {
            *ready = true;
            return true;
        }
        false
    }

    fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
    }

    fn turn_wrap_page(&mut self, delta: isize) {
        if let Some(Selected { content: Content::Text(doc), .. }) = &mut self.selected {
            let last = doc.wrap_page_count().saturating_sub(1);
            doc.wrap_page = doc.wrap_page.saturating_add_signed(delta).min(last);
        }
    }

    fn line_key(&self, i: usize) -> &str {
        match &self.selected {
            Some(Selected { content: Content::Text(doc), .. }) => doc.lines.get(i).map_or("", |l| l.key.as_str()),
            _ => "",
        }
    }

    fn is_copied(&self, now: i64) -> bool {
        self.copied_until.is_some_and(|(seq, until)| seq == self.sequence && now <= until)
    }
}

impl Worker {
    fn start(wake: Option<Waker>) -> Self {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (out, results) = mpsc::channel();
        // One reader per window. When selection changes rapidly, keep only the newest queued
        // request. The UI ignores a result for a file that is no longer selected.
        let _ = std::thread::Builder::new().name("feeder-file-preview".into()).spawn(move || {
            while let Ok(first) = inbox.recv() {
                let request = inbox.try_iter().last().unwrap_or(first);
                let result = read_file_preview(&request.root, Path::new(&request.path));
                if out.send((request.sequence, result)).is_err() {
                    break;
                }
                if let Some(wake) = &wake {
                    wake();
                }
            }
        });
        Self { requests, results }
    }
}

pub(super) fn view(s: &State, p: &Palette, k: &Kit) -> Option<El> {
    let preview = &s.files.preview;
    let selected = preview.selected.as_ref()?;
    let (directory, file) = selected.path.rsplit_once('/').unwrap_or(("", &selected.path));

    let mut meta_parts: Vec<String> = Vec::new();
    if !directory.is_empty() {
        meta_parts.push(directory.to_string());
    }
    let has_code = matches!(&selected.content, Content::Text(doc) if !doc.source.is_empty());
    if let Content::Text(doc) = &selected.content
        && !doc.source.is_empty()
    {
        if let Some(lang) = doc.lang {
            meta_parts.push(language_label(lang).to_string());
        }
        let unit = if doc.line_count == 1 { "line" } else { "lines" };
        meta_parts.push(format!("{} {unit}", doc.line_count));
    }

    let mut heading =
        col().grow(1.0).min_w(0.0).gap(2.0).child(text(file).font(style::NAME).color(p.text).truncate_middle().help_when_truncated(file));
    if !meta_parts.is_empty() {
        let subtitle = meta_parts.join(" · ");
        heading = heading.child(text(subtitle.clone()).font(style::META).color(p.text_3).truncate_middle().help_when_truncated(&subtitle));
    }

    let mut actions = row().items_center().gap(4.0);
    if has_code {
        let wrap_btn = Button::new(if preview.wrap { "Unwrap" } else { "Wrap" })
            .variant(if preview.wrap { ButtonVariant::Outline } else { ButtonVariant::GhostMuted })
            .control_size(ControlSize::Small)
            .on_press(|s: &mut State| s.files.preview.toggle_wrap())
            .build(k)
            .key("file-preview-wrap");
        let copied = preview.is_copied(s.now);
        let copy_btn = Button::new(if copied { "Copied" } else { "Copy" })
            .variant(ButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .on_press_tx(|s: &mut State, tx: &mut Tx| {
                if let Some(Selected { content: Content::Text(doc), .. }) = &s.files.preview.selected {
                    tx.copy(doc.source.clone());
                    s.files.preview.copied_until = Some((s.files.preview.sequence, s.now + 2));
                }
            })
            .build(k)
            .key("file-preview-copy");
        actions = actions.child(wrap_btn).child(copy_btn);
    }
    actions = actions.child(
        Button::new("Close Preview")
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .on_press_tx(|s: &mut State, tx: &mut Tx| {
                s.files.preview.close();
                tx.focus_when_shown("files-list", false);
            })
            .build(k)
            .key("close-file-preview"),
    );

    let header = row().w_full().items_center().gap(12.0).px(16.0).py(10.0).child(heading).child(actions);
    let notice = |message: &str| el().grow(1.0).center().padding(16.0).child(text(message).font(style::BODY).color(p.text_2).wrap());
    let body = match &selected.content {
        Content::Loading => notice("Loading file…"),
        Content::Notice(message) => notice(message),
        Content::Image { image, .. } if image.error().is_some() => notice("Couldn’t display this image."),
        Content::Image { ready: false, .. } => notice("Loading image…"),
        Content::Image { image, .. } => el().grow(1.0).min_h(0.0).w_full().padding(12.0).pixels_fit(image.pixels(), SurfaceFit::Fit),
        Content::Text(doc) if doc.source.is_empty() => notice("This file is empty."),
        Content::Text(doc) => code_view(preview, doc, p, k),
    };
    Some(
        col()
            .key("file-preview")
            .label("File preview")
            .min_w(0.0)
            .min_h(0.0)
            .grow(1.0)
            .child(header)
            .child(rune::rule(1.0, p.hairline))
            .child(body),
    )
}

fn code_view(preview: &Preview, doc: &CodeDoc, p: &Palette, k: &Kit) -> El {
    let syntax = SyntaxTheme::from_kit(k);
    let gutter_w = gutter_width(doc.digits);
    let gutter_bg = if p.dark { k.c.layer_01 } else { p.bg };
    let line_num_color = p.text_3.alpha(if p.dark { 0.72 } else { 0.75 });
    let code_font = TextStyle::new(CODE_FONT_SIZE, 400.0).lh(CODE_LINE_H).mono();
    let gutter_font = TextStyle::new(GUTTER_FONT_SIZE, 400.0).lh(CODE_LINE_H).mono().features("'tnum' 1");
    let wrap = preview.wrap;
    let p_copy = *p;

    if !wrap {
        let content_w = gutter_w + 1.0 + (doc.max_cols as f32) * MONO_ADVANCE + 48.0;
        let version = (preview.sequence << 2) | (u64::from(p.dark) << 1);
        let cap = |key: &'static str| {
            let gutter_cap = row()
                .self_stretch()
                .items_stretch()
                .sticky_left(0.0)
                .child(el().w(gutter_w).h(6.0).fill(gutter_bg).a11y_hidden())
                .child(el().w(1.0).h(6.0).fill(p.hairline).a11y_hidden());
            row().key(key).w_full().min_w(content_w).h(6.0).child(gutter_cap)
        };
        let list = col()
            .key("code-lines")
            .w_full()
            .min_w(content_w)
            .pb(8.0)
            .growth(GrowthSpec::default())
            .selection_group(true)
            .role("list")
            .label("Code")
            .virtual_rows(
                doc.lines.len(),
                CODE_LINE_H,
                |s: &State, i| Cow::Borrowed(s.files.preview.line_key(i)),
                move |s: &State, i| {
                    let Some(Selected { content: Content::Text(doc), .. }) = &s.files.preview.selected else {
                        return el();
                    };
                    let Some(line) = doc.lines.get(i) else { return el() };
                    code_line_row(doc, line, &syntax, &p_copy, gutter_w, gutter_bg, line_num_color, code_font, gutter_font, false)
                },
            )
            .virtual_rows_version(version);

        let scroll_key = format!("file-preview-scroll-{}", preview.sequence);
        let viewport = col()
            .w_full()
            .min_h(0.0)
            .grow(1.0)
            .child(cap("code-top-cap"))
            .child(list)
            .scroll(ScrollSpec { stick_bottom: false })
            .scroll_xy();
        return scroll_bar(k, &format!("{scroll_key}-area"), &scroll_key, viewport).dir(Direction::Ltr).w_full().grow(1.0).min_h(0.0);
    }

    let page_count = doc.wrap_page_count();
    let start = doc.wrap_page * WRAP_PAGE_LINES;
    let end = (start + WRAP_PAGE_LINES).min(doc.lines.len());
    let rows = rune::memo("file-preview-wrap-lines", (preview.sequence, doc.wrap_page, p.dark), || {
        let cap = |key: &'static str| {
            row()
                .key(key)
                .w_full()
                .h(6.0)
                .child(el().w(gutter_w).h(6.0).fill(gutter_bg).a11y_hidden())
                .child(el().w(1.0).h(6.0).fill(p.hairline).a11y_hidden())
        };
        let mut list = col().w_full().selection_group(true).role("list").label("Code").child(cap("wrap-top-cap"));
        for line in &doc.lines[start..end] {
            list = list.child(
                code_line_row(doc, line, &syntax, &p_copy, gutter_w, gutter_bg, line_num_color, code_font, gutter_font, true)
                    .key(line.key.clone()),
            );
        }
        list.child(cap("wrap-bot-cap"))
    });

    let scroll_key = format!("file-preview-wrap-{}-{}", preview.sequence, doc.wrap_page);
    let viewport = col().w_full().min_h(0.0).grow(1.0).child(rows).scroll(ScrollSpec { stick_bottom: false });
    let mut out = col()
        .dir(Direction::Ltr)
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(scroll_bar(k, &format!("{scroll_key}-area"), &scroll_key, viewport).w_full().grow(1.0).min_h(0.0));
    if page_count > 1 {
        let first_num = doc.lines[start].number.unwrap_or(start + 1);
        let last_num = doc.lines[end - 1].number.unwrap_or(end);
        out = out.child(rune::rule(1.0, p.hairline)).child(
            row()
                .justify_between()
                .items_center()
                .px(12.0)
                .py(6.0)
                .child(
                    Button::new("Previous")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Small)
                        .disabled(doc.wrap_page == 0)
                        .on_press(|s: &mut State| s.files.preview.turn_wrap_page(-1))
                        .build(k),
                )
                .child(text(format!("Lines {first_num}–{last_num} of {}", doc.line_count)).font(style::META).color(p.text_3))
                .child(
                    Button::new("Next")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Small)
                        .disabled(doc.wrap_page + 1 == page_count)
                        .on_press(|s: &mut State| s.files.preview.turn_wrap_page(1))
                        .build(k)
                        .key("file-preview-next"),
                ),
        );
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn code_line_row(
    doc: &CodeDoc,
    line: &CodeLine,
    syntax: &SyntaxTheme,
    p: &Palette,
    gutter_w: f32,
    gutter_bg: rune::Color,
    line_num_color: rune::Color,
    code_font: TextStyle,
    gutter_font: TextStyle,
    wrap: bool,
) -> El {
    let raw = &doc.source[line.range.clone()];
    let spans: Vec<Span> = line
        .highlights
        .iter()
        .map(|(r, paint)| {
            let mut sp = Span::new(r.start, r.end);
            sp.color = Some(syntax.paint(*paint));
            if let Paint::Kind(kind) = paint {
                sp.weight = syntax.weight(*kind);
            }
            sp
        })
        .collect();
    let (shown, spans, pads) = tabbed(raw, spans, wrap);
    let mut code = rich(shown, spans).font(code_font).color(p.text).selectable(true).selection_color(p.selection);
    code = if wrap { code.w_full() } else { code.single_line() };
    for pad in pads {
        code = code.uncopied(pad);
    }

    let mut gutter =
        row().w(gutter_w).min_h(CODE_LINE_H).self_stretch().pl(12.0).pr(10.0).justify_end().items_start().fill(gutter_bg).a11y_hidden();
    if let Some(n) = line.number {
        gutter = gutter.child(text(n.to_string()).font(gutter_font).color(line_num_color));
    }
    let divider = col().w(1.0).self_stretch().fill(p.hairline).a11y_hidden();
    let mut sticky_gutter = row().self_stretch().items_stretch().child(gutter).child(divider);
    if !wrap {
        sticky_gutter = sticky_gutter.sticky_left(0.0);
    }

    row()
        .w_full()
        .min_h(CODE_LINE_H)
        .items_start()
        .hover(p.hover)
        .child(sticky_gutter)
        .child(col().grow(1.0).min_w(0.0).pl(14.0).pr(24.0).child(code))
}

fn gutter_width(digits: usize) -> f32 {
    (digits as f32) * 7.2 + 24.0
}

fn decimal_digits(mut n: usize) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

fn display_columns(line: &str) -> usize {
    let mut cols = 0usize;
    for ch in line.chars() {
        if ch == '\t' {
            cols += 4 - (cols % 4);
        } else {
            cols += 1;
        }
    }
    cols
}

/// Expands tabs to 4-column stops (and adds soft break points when wrapped) without changing what
/// gets copied to the clipboard.
fn tabbed(code: &str, mut spans: Vec<Span>, wrap: bool) -> (String, Vec<Span>, Vec<Range<usize>>) {
    if !code.contains('\t') && !(wrap && code.contains(['.', '(', '[', '/'])) {
        return (code.to_owned(), spans, Vec::new());
    }
    let mut shown = String::with_capacity(code.len() + 8);
    let mut shifts: Vec<(usize, usize)> = Vec::new();
    let mut pads = Vec::new();
    let mut col = 0;
    let mut chars = code.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        shown.push(ch);
        col += 1;
        let start = shown.len();
        if ch == '\t' {
            let pad = 3 - (col - 1) % 4;
            shown.extend(std::iter::repeat_n(' ', pad));
            col += pad;
        } else if wrap && matches!(ch, '.' | '(' | '[' | '/') && chars.peek().is_some_and(|(_, c)| !c.is_whitespace()) {
            shown.push('\u{200B}');
        }
        if shown.len() > start {
            pads.push(start..shown.len());
            shifts.push((i + ch.len_utf8(), shifts.last().map_or(0, |s| s.1) + shown.len() - start));
        }
    }
    let shift = |b: u32| match shifts.partition_point(|s| s.0 <= b as usize) {
        0 => b,
        n => b + shifts[n - 1].1 as u32,
    };
    for sp in &mut spans {
        (sp.start, sp.end) = (shift(sp.start), shift(sp.end));
    }
    (shown, spans, pads)
}

fn language_label(lang: &str) -> &str {
    match lang {
        "typescript" | "angular-ts" => "TypeScript",
        "tsx" => "TSX",
        "javascript" => "JavaScript",
        "jsx" => "JSX",
        "rust" => "Rust",
        "python" => "Python",
        "swift" => "Swift",
        "go" => "Go",
        "json" | "jsonc" | "json5" | "jsonl" => "JSON",
        "toml" => "TOML",
        "yaml" | "yml" => "YAML",
        "markdown" | "mdx" => "Markdown",
        "html" | "angular-html" => "HTML",
        "css" => "CSS",
        "scss" => "SCSS",
        "sql" => "SQL",
        "zsh" => "Shell",
        "dockerfile" => "Dockerfile",
        "makefile" => "Makefile",
        "c" => "C",
        "cpp" => "C++",
        "objective-cpp" => "Objective-C++",
        "java" => "Java",
        "kotlin" | "kts" => "Kotlin",
        "ruby" => "Ruby",
        "php" => "PHP",
        "csharp" => "C#",
        "zig" => "Zig",
        "diff" => "Diff",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_doc_splits_lines_highlights_multiline_tokens_and_bounds_huge_lines() {
        let src = "import { x } from \"a\";\nconst s = `line1\nline2`;\nexport const n = 42;\n";
        let doc = CodeDoc::from_source("src/commands/bisect.ts", src.into());
        assert_eq!(doc.lang, Some("typescript"));
        assert_eq!(doc.line_count, 4);
        assert_eq!(doc.lines.len(), 4);
        assert_eq!(doc.lines.iter().map(|l| l.number).collect::<Vec<_>>(), vec![Some(1), Some(2), Some(3), Some(4)]);
        assert!(!doc.lines[0].highlights.is_empty(), "TypeScript keywords and strings highlight");
        assert!(!doc.lines[2].highlights.is_empty(), "Multi-line template literal keeps highlighting across lines");

        let huge = format!("{}\n{}", "🙂".repeat(3000), "tail");
        let doc = CodeDoc::from_source("bundle.txt", huge.clone());
        assert_eq!(doc.line_count, 2);
        assert!(doc.lines.len() > 2, "Huge single line splits into bounded chunks");
        assert_eq!(doc.lines[0].number, Some(1));
        assert_eq!(doc.lines[1].number, None);
        assert_eq!(doc.lines.last().unwrap().number, Some(2));
        let rebuilt: String = doc.lines.iter().take(doc.lines.len() - 1).map(|l| &doc.source[l.range.clone()]).collect();
        assert_eq!(rebuilt, "🙂".repeat(3000));
    }

    #[test]
    fn tabs_expand_to_four_columns_without_shifting_copied_text() {
        let (shown, spans, pads) = tabbed("\tlet x = 1;", vec![Span::new(1, 4)], false);
        assert_eq!(shown, "\t   let x = 1;");
        assert_eq!(&shown[spans[0].start as usize..spans[0].end as usize], "let");
        let copied: String = shown.char_indices().filter(|(i, _)| !pads.iter().any(|r| r.contains(i))).map(|(_, c)| c).collect();
        assert_eq!(copied, "\tlet x = 1;");
    }
}
