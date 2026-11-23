use clap::Subcommand;
use feeder_core::{Kind, NOTEBOOK_GUIDE, OnSimilar, Page, PageDraft, PageFilter, Standing, Store, detect_project, now_ms};
use serde_json::{Value, json};

#[derive(Subcommand)]
pub enum NotebookCmd {
    #[command(about = "Search pages by words in the title, tags, rule and story; best match first.")]
    Search {
        query: Vec<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        topic: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(help = "Include archived pages.")]
        #[arg(long)]
        all: bool,
        #[arg(short = 'n', long, default_value_t = 8)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "List pages, the most recently revised first.")]
    List {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        topic: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(help = "Only pages that came from this project.")]
        #[arg(short, long)]
        project: Option<String>,
        #[arg(help = "Include archived pages.")]
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Show one page in full.")]
    Show {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Write a new Markdown note, or revise one with `--id`. `feeder notebook guide` explains how.")]
    #[command(after_long_help = NOTEBOOK_GUIDE)]
    Write {
        #[arg(help = "Revise this page instead of adding a new one.")]
        #[arg(long)]
        id: Option<i64>,
        #[arg(help = "Optional kind (`note`, `fix`, `pitfall`, `pattern`, `decision`). Defaults to `note`.")]
        #[arg(long)]
        kind: Option<String>,
        #[arg(help = "Optional category/topic (`rust`, `react`, `nextjs`, `design`, `general`, or a custom slug).")]
        #[arg(long)]
        topic: Option<String>,
        #[arg(long)]
        title: String,
        #[arg(help = "Optional comma-separated tags: `rust,sqlite`.")]
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(help = "Freeform Markdown body. Pass `-` or omit with `--file` / piped stdin.")]
        #[arg(long)]
        body: Option<String>,
        #[arg(help = "Read the Markdown body from a `.md` file.")]
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,
        #[arg(help = "Legacy: what happened (composed into Markdown if `--body` is omitted).")]
        #[arg(long)]
        what: Option<String>,
        #[arg(help = "Legacy: why / root cause (composed into Markdown if `--body` is omitted).")]
        #[arg(long)]
        why: Option<String>,
        #[arg(help = "Legacy: rule to follow (composed into Markdown if `--body` is omitted).")]
        #[arg(long = "do")]
        rule: Option<String>,
        #[arg(help = "The project it came from. Defaults to the current folder's project, if it has a feed.")]
        #[arg(short, long)]
        project: Option<String>,
        #[arg(help = "File it even if a similar page exists (only for a genuinely different note).")]
        #[arg(long)]
        new: bool,
        #[arg(short, long)]
        agent: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Record that a page helped, so useful pages rise in search.")]
    Helped { id: i64 },
    #[command(about = "Archive an outdated page (kept for the record, hidden from search).")]
    Archive { id: i64 },
    #[command(about = "Bring an archived page back.")]
    Restore { id: i64 },
    #[command(about = "Delete a page for good.")]
    Rm {
        id: i64,
        #[arg(help = "Required: confirms the deletion.")]
        #[arg(long)]
        yes: bool,
    },
    #[command(about = "High-level categories/topics in use, with page counts and top tags.")]
    Topics {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Tags in use, with how many pages carry each.")]
    Tags {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "How to use the notebook well (the same guide agents get).")]
    Guide,
}

pub fn run(store: &Store, cmd: Option<NotebookCmd>, author: &str) -> Result<(), String> {
    let cmd = cmd.unwrap_or(NotebookCmd::List { kind: None, topic: None, tag: None, project: None, all: false, json: false });
    let now = now_ms();
    match cmd {
        NotebookCmd::Search { query, kind, topic, tag, all, limit, json } => {
            let filter = PageFilter { kind: parse_kind(kind)?, topic, tag, project_id: None, include_archived: all };
            let query = query.join(" ");
            let hits = store.search_pages(&query, &filter, limit).map_err(|e| e.to_string())?;
            if json {
                println!("{}", json!({ "query": query, "pages": hits.iter().map(|p| page_json(store, p)).collect::<Vec<_>>() }));
            } else if hits.is_empty() {
                println!("Nothing in the notebook matches “{query}”.");
            } else {
                for p in &hits {
                    println!("{}", p.summary_line());
                }
                println!("\nOpen one: feeder notebook show <id>");
            }
        }
        NotebookCmd::List { kind, topic, tag, project, all, json } => {
            let project_id = match project {
                Some(name) => Some(store.find_project(&name).map_err(|e| e.to_string())?.ok_or(format!("no project called {name}"))?.id),
                None => None,
            };
            let filter = PageFilter { kind: parse_kind(kind)?, topic, tag, project_id, include_archived: all };
            let pages = store.pages(&filter).map_err(|e| e.to_string())?;
            if json {
                println!("{}", Value::Array(pages.iter().map(|p| page_json(store, p)).collect()));
            } else if pages.is_empty() {
                println!(
                    "The notebook is empty so far. Write a note in Feeder or via `feeder notebook write`; `feeder notebook guide` explains how."
                );
            } else {
                for p in &pages {
                    let archived = if p.is_archived() { "  (archived)" } else { "" };
                    println!("#{:<4} {:<14} {}{archived}", p.id, p.topic_label(), p.title);
                }
            }
        }
        NotebookCmd::Show { id, json } => {
            let page = store.page(id).map_err(|e| e.to_string())?.ok_or(format!("no notebook page {id}"))?;
            if json {
                println!("{}", page_json(store, &page));
            } else {
                print!("{}", page.to_markdown(project_name(store, &page).as_deref(), now));
            }
        }
        NotebookCmd::Write { id, kind, topic, title, tags, body, file, what, why, rule, project, new, agent, json } => {
            let kind = parse_kind(kind)?.unwrap_or(Kind::Note);
            let draft = if let Some(path) = file {
                let text = std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
                PageDraft { kind, topic, title, tags, body: text }
            } else if let Some(b) = body {
                let text = if b.trim() == "-" {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).map_err(|e| e.to_string())?;
                    buf
                } else {
                    b
                };
                PageDraft { kind, topic, title, tags, body: text }
            } else if what.is_some() || rule.is_some() {
                PageDraft::from_legacy(kind, topic, title, tags, what.unwrap_or_default(), why, rule.unwrap_or_default())
            } else if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).map_err(|e| e.to_string())?;
                PageDraft { kind, topic, title, tags, body: buf }
            } else {
                return Err("provide `--body <markdown>`, `--file <path.md>`, or pipe Markdown on stdin".into());
            };
            let author = agent.map(|a| feeder_core::agent_display_name(&a)).unwrap_or_else(|| author.to_string());
            let page = match id {
                Some(id) => store.revise_page(id, draft, &author),
                None => {
                    let project_id = origin_project(store, project.as_deref())?;
                    let on_similar = if new { OnSimilar::Allow } else { OnSimilar::Refuse };
                    store.create_page(draft, &author, project_id, on_similar)
                }
            }
            .map_err(|e| e.to_string())?;
            if json {
                println!("{}", page_json(store, &page));
            } else {
                let verb = if page.revision > 1 { "Revised" } else { "Wrote" };
                println!("{verb} page #{}: {}", page.id, page.title);
            }
        }
        NotebookCmd::Helped { id } => {
            let page = store.mark_page_helped(id).map_err(|e| e.to_string())?;
            println!("Noted: #{} has helped {}×.", page.id, page.helped);
        }
        NotebookCmd::Archive { id } => {
            store.set_page_standing(id, Standing::Archived).map_err(|e| e.to_string())?;
            println!("Archived page #{id}.");
        }
        NotebookCmd::Restore { id } => {
            store.set_page_standing(id, Standing::Current).map_err(|e| e.to_string())?;
            println!("Restored page #{id}.");
        }
        NotebookCmd::Rm { id, yes } => {
            let page = store.page(id).map_err(|e| e.to_string())?.ok_or(format!("no notebook page {id}"))?;
            if !yes {
                return Err(format!("this deletes page #{id} \"{}\" for good; run again with --yes (or archive it)", page.title));
            }
            store.delete_page(id).map_err(|e| e.to_string())?;
            println!("Deleted page #{id}.");
        }
        NotebookCmd::Topics { json } => {
            let topics = store.page_topics().map_err(|e| e.to_string())?;
            if json {
                println!(
                    "{}",
                    Value::Array(
                        topics
                            .iter()
                            .map(|t| json!({
                                "topic": t.topic,
                                "label": t.label,
                                "pages": t.pages,
                                "tags": t.tags.iter().map(|tg| json!({ "tag": tg.tag, "pages": tg.pages })).collect::<Vec<_>>()
                            }))
                            .collect()
                    )
                );
            } else if topics.is_empty() {
                println!("No topics yet.");
            } else {
                for t in topics {
                    let tags: Vec<String> = t.tags.iter().take(4).map(|tg| format!("#{}", tg.tag)).collect();
                    println!("{:<16} {:>3}  {}", t.label, t.pages, tags.join(" "));
                }
            }
        }
        NotebookCmd::Tags { json } => {
            let tags = store.page_tags().map_err(|e| e.to_string())?;
            if json {
                println!("{}", Value::Array(tags.iter().map(|t| json!({ "tag": t.tag, "pages": t.pages })).collect()));
            } else if tags.is_empty() {
                println!("No tags yet.");
            } else {
                for t in tags {
                    println!("#{:<24} {}", t.tag, t.pages);
                }
            }
        }
        NotebookCmd::Guide => print!("{NOTEBOOK_GUIDE}"),
    }
    Ok(())
}

fn parse_kind(kind: Option<String>) -> Result<Option<Kind>, String> {
    kind.map(|k| k.parse::<Kind>()).transpose().map_err(|e| e.to_string())
}

fn origin_project(store: &Store, name: Option<&str>) -> Result<Option<i64>, String> {
    if let Some(name) = name {
        let p = store.find_project(name).map_err(|e| e.to_string())?.ok_or(format!("no project called {name}"))?;
        return Ok(Some(p.id));
    }
    let Ok(here) = std::env::current_dir() else { return Ok(None) };
    Ok(store.find_detected(&detect_project(&here)).map_err(|e| e.to_string())?.map(|p| p.id))
}

pub(crate) fn project_name(store: &Store, page: &Page) -> Option<String> {
    page.project_id.and_then(|id| store.project(id).ok().flatten()).map(|p| p.name)
}

pub(crate) fn page_json(store: &Store, p: &Page) -> Value {
    json!({
        "id": p.id,
        "kind": p.kind.as_str(),
        "topic": p.topic,
        "topic_label": p.topic_label(),
        "title": p.title,
        "tags": p.tags,
        "body": p.body,
        "excerpt": p.excerpt(160),
        "project": project_name(store, p),
        "agent": p.agent,
        "revision": p.revision,
        "helped": p.helped,
        "archived": p.is_archived(),
        "created_at": p.created_at,
        "updated_at": p.updated_at,
    })
}
