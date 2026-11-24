mod gardener;
mod mcp;
mod notebook;
mod web;

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use feeder_core::{
    FeedUpdate, GUIDE, NewUpdate, Project, Store, TodoItem, agent_display_name, agent_from_env, ago, detect_git_meta, detect_project,
    install_post_commit_hook, latest_commit_summary, now_ms, remove_post_commit_hook, setup,
};
use serde_json::json;

#[derive(Parser)]
#[command(name = "feeder", version, about = "Leave and read short project updates for Feeder")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Post an update to a project's feed (1–3 short blocks). `feeder guide` shows how to write one.")]
    #[command(after_long_help = GUIDE)]
    Post {
        #[arg(help = "The update. Omit it, or pass `-`, to read it from stdin.")]
        message: Vec<String>,
        #[arg(help = "Project name. Defaults to the git repository (or folder) of --dir.")]
        #[arg(short, long)]
        project: Option<String>,
        #[arg(help = "Who's posting, e.g. \"Claude Code\". Detected from the tool you run it in.")]
        #[arg(short, long)]
        agent: Option<String>,
        #[arg(help = "Screenshot(s) or short video clip(s) to attach (repeat up to 8× for a carousel).")]
        #[arg(short, long)]
        screenshot: Vec<PathBuf>,
        #[arg(help = "Git branch name. Defaults to the current branch of --dir.")]
        #[arg(short, long)]
        branch: Option<String>,
        #[arg(help = "Optional diff stat override (auto-detected from git when omitted).")]
        #[arg(long)]
        diff_stat: Option<String>,
        #[arg(help = "The folder to detect the project from. Defaults to the current folder.")]
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Edit an existing update's text body.")]
    Edit {
        #[arg(help = "Update id.")]
        id: i64,
        #[arg(help = "The new update text. Omit it, or pass `-`, to read it from stdin.")]
        message: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Delete a single update (and its stored media).")]
    #[command(alias = "rm-update")]
    DeleteUpdate {
        #[arg(help = "Update id.")]
        id: i64,
        #[arg(help = "Required: confirms the deletion.")]
        #[arg(long)]
        yes: bool,
    },
    #[command(about = "Show a project's latest updates, newest first.")]
    Feed {
        #[arg(help = "Project name. Defaults to the project of the current folder.")]
        project: Option<String>,
        #[arg(short = 'n', long, default_value_t = 5)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Reply to an update card (human answer/decision or agent follow-up).")]
    Reply {
        #[arg(help = "Update id.")]
        id: i64,
        #[arg(help = "Reply message. Omit or pass `-` to read from stdin.")]
        message: Vec<String>,
        #[arg(help = "Author name (defaults to \"You\" for human replies or detected agent with --from-agent).")]
        #[arg(short, long)]
        agent: Option<String>,
        #[arg(help = "Mark this reply as coming from an agent instead of the user.")]
        #[arg(long)]
        from_agent: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Show replies (optionally only unread human replies) and mark them read.")]
    Replies {
        #[arg(short, long)]
        project: Option<String>,
        #[arg(long)]
        unread: bool,
        #[arg(long)]
        mark_read: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "View or update a project's pinned brief (`Goal / Focus / Commands`).")]
    Brief {
        #[arg(help = "New brief text. Omit to print the current brief.")]
        text: Vec<String>,
        #[arg(short, long)]
        project: Option<String>,
        #[arg(help = "Clear the pinned brief.")]
        #[arg(long)]
        clear: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "The shared library of lessons: fixes, pitfalls, patterns and decisions across projects.")]
    #[command(visible_aliases = ["library", "lib", "nb", "kb"])]
    Notebook {
        #[command(subcommand)]
        action: Option<notebook::NotebookCmd>,
    },
    #[command(about = "Flag Notebook knowledge that is missing, wrong or outdated for Curator to review.")]
    Flag {
        #[arg(help = "What's missing or wrong.")]
        note: Vec<String>,
        #[arg(help = "Flag a Feed thread and its replies for Curator to review.")]
        #[arg(long, conflicts_with_all = ["note", "kind", "page", "suggest", "suggest_file", "project", "shared"])]
        update: Option<i64>,
        #[arg(help = "`missing` (default), `wrong` or `outdated`.")]
        #[arg(long)]
        kind: Option<String>,
        #[arg(help = "The page that's wrong or outdated.")]
        #[arg(long)]
        page: Option<i64>,
        #[arg(help = "The correct information, if you know it.")]
        #[arg(long, conflicts_with = "suggest_file")]
        suggest: Option<String>,
        #[arg(help = "Read a Markdown suggestion draft from a file.")]
        #[arg(long, conflicts_with = "suggest")]
        suggest_file: Option<PathBuf>,
        #[arg(help = "The project you're working in. Defaults to the current folder's project.")]
        #[arg(short, long)]
        project: Option<String>,
        #[arg(help = "Flag a cross-project shared gap instead of tying it to the current folder's project.")]
        #[arg(long, conflicts_with_all = ["project", "update"])]
        shared: bool,
        #[arg(short, long)]
        agent: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "List open flags (or all with --all), or dismiss one.")]
    Flags {
        #[arg(long)]
        all: bool,
        #[arg(help = "Dismiss this flag.")]
        #[arg(long)]
        dismiss: Option<i64>,
        #[arg(help = "Why it's dismissed.")]
        #[arg(long, requires = "dismiss")]
        reason: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Ask a Notebook question or review flagged notes with Curator.")]
    #[command(name = "curator", visible_alias = "gardener")]
    Gardener {
        #[command(subcommand)]
        action: gardener::GardenerCmd,
    },
    #[command(about = "Manage a project's optional to-do list.")]
    Todo {
        #[command(subcommand)]
        action: Option<TodoCmd>,
        #[arg(short, long)]
        project: Option<String>,
        #[arg(help = "Show only open (unchecked) to-dos.")]
        #[arg(long)]
        open: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Emit or clear a live \"working\" pulse on a project's orb.")]
    Pulse {
        #[arg(help = "Short activity status, e.g. \"Refactoring auth\".")]
        status: Vec<String>,
        #[arg(short, long)]
        project: Option<String>,
        #[arg(short, long)]
        agent: Option<String>,
        #[arg(short, long)]
        branch: Option<String>,
        #[arg(help = "Time-to-live in seconds (default 120).")]
        #[arg(long, default_value_t = 120)]
        ttl: u64,
        #[arg(help = "Clear the pulse for this project and agent.")]
        #[arg(long)]
        clear: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Install (or remove) a quiet git post-commit hook in a repository.")]
    InstallHook {
        #[arg(help = "Repository folder (defaults to current directory).")]
        dir: Option<PathBuf>,
        #[arg(help = "Remove the Feeder hook instead of installing it.")]
        #[arg(long)]
        remove: bool,
    },
    #[command(about = "Internal handler invoked by git's post-commit hook.")]
    #[command(hide = true)]
    HookPostCommit { dir: Option<PathBuf> },
    #[command(about = "Start the local / Tailscale companion HTTP server.")]
    Serve {
        #[arg(help = "Address and port to bind (e.g. 127.0.0.1:7331 or your Tailscale IP).")]
        #[arg(long, default_value = "127.0.0.1:7331")]
        bind: String,
        #[arg(help = "Bearer token required when binding beyond localhost.")]
        #[arg(long)]
        token: Option<String>,
    },
    #[command(about = "List projects, most recently active first.")]
    Projects {
        #[arg(help = "Show only archived projects.")]
        #[arg(long)]
        archived: bool,
        #[arg(help = "Show both active and archived projects.")]
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Add a project without posting to it, optionally under a nickname.")]
    Add {
        #[arg(help = "The project folder. Defaults to the current folder's git repository.")]
        dir: Option<PathBuf>,
        #[arg(help = "A nickname to show instead of the folder name.")]
        #[arg(short, long)]
        name: Option<String>,
    },
    #[command(about = "Give a project a nickname (found by its old name or folder name). \"\" restores the folder name.")]
    Rename { project: String, name: String },
    #[command(about = "Hide a project from the active Home shelf and global Feed without deleting its history.")]
    Archive {
        #[arg(help = "Project name. Defaults to the project of the current folder.")]
        project: Option<String>,
    },
    #[command(about = "Restore an archived project to the active Home shelf and global Feed.")]
    Unarchive {
        #[arg(help = "Project name. Defaults to the project of the current folder.")]
        project: Option<String>,
    },
    #[command(about = "Pin a project so it stays anchored first on Home.")]
    Pin {
        #[arg(help = "Project name. Defaults to the project of the current folder.")]
        project: Option<String>,
    },
    #[command(about = "Unpin a project.")]
    Unpin {
        #[arg(help = "Project name. Defaults to the project of the current folder.")]
        project: Option<String>,
    },
    #[command(about = "Search updates across projects.")]
    Search {
        query: String,
        #[arg(short, long)]
        project: Option<String>,
        #[arg(long)]
        archived: bool,
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Mark a project (or --all active projects) as read.")]
    Seen {
        #[arg(help = "Project name. Defaults to the project of the current folder.")]
        project: Option<String>,
        #[arg(help = "Mark every active project as read.")]
        #[arg(long)]
        all: bool,
    },
    #[command(about = "Dismiss the \"needs you\" flag on a project's latest update or a specific update id.")]
    Dismiss {
        #[arg(help = "Project name. Defaults to the project of the current folder.")]
        project: Option<String>,
        #[arg(help = "Specific update id to dismiss instead of the project's latest update.")]
        #[arg(long)]
        id: Option<i64>,
    },
    #[command(about = "Delete a project, its feed and its screenshots.")]
    Remove {
        project: String,
        #[arg(help = "Required: confirms the deletion.")]
        #[arg(long)]
        yes: bool,
    },
    #[command(about = "Run the MCP server on stdio (agent tools start this themselves; it's local only).")]
    Mcp {
        #[arg(help = "Offer only the Notebook gardener's tools (Feeder starts this for a gardener session).")]
        #[arg(long, hide = true, requires = "gardener_run")]
        gardener: bool,
        #[arg(long, hide = true, requires = "gardener")]
        gardener_run: Option<i64>,
    },
    #[command(
        about = "Connect Feeder to your agent tools: the MCP server, plus the writing skill.",
        long_about = "Connect Feeder to your agent tools: the MCP server, plus the writing skill.\n\nWith no tools named it lists them; `--all` connects every one found on this Mac.\n`--remove` disconnects instead; `--remove --all` also deletes the skills, which is\neverything to undo before deleting Feeder.\nTools: claude-code, claude-desktop, codex, cursor, antigravity, t3-code, opencode."
    )]
    Setup {
        tools: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(help = "Print the config each tool needs instead of changing anything.")]
        #[arg(long)]
        print: bool,
        #[arg(help = "Disconnect the named tools (or, with `--all`, every tool) instead of connecting them.")]
        #[arg(long, conflicts_with = "print")]
        remove: bool,
    },
    #[command(about = "How to write a good update (the same guide agents get).")]
    Guide,
    #[command(about = "Print where the store lives.")]
    Where,
}

#[derive(Subcommand)]
enum TodoCmd {
    #[command(about = "List to-dos for a project (default when no subcommand is given).")]
    List {
        #[arg(short, long)]
        project: Option<String>,
        #[arg(help = "Show only open (unchecked) to-dos.")]
        #[arg(long)]
        open: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Add a to-do item to a project's checklist.")]
    Add {
        #[arg(help = "To-do title (omit or pass `-` to read from stdin).")]
        title: Vec<String>,
        #[arg(short, long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Mark a to-do item done (or pass --reopen to uncheck it).")]
    Done {
        #[arg(help = "To-do id.")]
        id: i64,
        #[arg(help = "Reopen a completed to-do instead of marking it done.")]
        #[arg(long)]
        reopen: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Rename an existing to-do item.")]
    Edit {
        #[arg(help = "To-do id.")]
        id: i64,
        #[arg(help = "New to-do title.")]
        title: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Delete a single to-do item.")]
    #[command(alias = "delete")]
    Rm {
        #[arg(help = "To-do id.")]
        id: i64,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Clear completed to-dos (or --all to clear the whole checklist).")]
    Clear {
        #[arg(short, long)]
        project: Option<String>,
        #[arg(help = "Remove every to-do for this project, including open ones.")]
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Mcp { gardener_run, .. } => mcp::serve(gardener_run.map_or(mcp::Role::Agent, mcp::Role::Gardener)),
        Command::Serve { bind, token } => web::serve(&bind, token.as_deref()),
        Command::Guide => {
            print!("{GUIDE}");
            Ok(())
        }
        Command::Setup { tools, all, print, remove } => run_setup(tools, all, print, remove),
        Command::Where => {
            println!("{}", feeder_core::data_dir().display());
            Ok(())
        }
        command => run(command),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("feeder: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    match command {
        Command::Post { message, project, agent, screenshot, branch, diff_stat, dir, json } => {
            let body = read_body(message)?;
            let dir_explicit = dir.is_some();
            let dir = match dir {
                Some(d) => d,
                None => std::env::current_dir().map_err(|e| e.to_string())?,
            };
            let explicit_project = project.is_some();
            let project = match project {
                Some(name) => store.project_named(&name, None),
                None => store.project_at(&detect_project(&dir)),
            }
            .map_err(|e| e.to_string())?;
            let agent = agent_display_name(&agent.unwrap_or_else(default_agent));
            let screenshots: Vec<PathBuf> = screenshot.into_iter().map(|p| absolute(&p)).collect();
            let git_dir = if dir_explicit || !explicit_project {
                Some(dir)
            } else {
                project.path.as_deref().map(PathBuf::from).filter(|p| p.exists())
            };
            let meta = git_dir.as_deref().map(detect_git_meta).unwrap_or_default();
            let branch = branch
                .and_then(|b| {
                    let t = b.trim().to_string();
                    (!t.is_empty()).then_some(t)
                })
                .or(meta.branch);
            let diff_stat = diff_stat.or(meta.diff_stat);
            let commit_sha = meta.commit_sha;
            let posted = store
                .post(project.id, NewUpdate { agent, body, screenshot: None, screenshots, branch, diff_stat, commit_sha })
                .map_err(|e| e.to_string())?;
            if json {
                println!("{}", json!({ "project": project_json(&project), "update": update_json(&posted) }));
            } else {
                println!("Posted to {} as {}.", project.name, posted.agent);
            }
            Ok(())
        }
        Command::Edit { id, message, json } => {
            let body = read_body(message)?;
            let updated = store.edit_update(id, &body).map_err(|e| e.to_string())?;
            if json {
                println!("{}", update_json(&updated));
            } else {
                println!("Updated #{id}.");
            }
            Ok(())
        }
        Command::DeleteUpdate { id, yes } => {
            if !yes {
                return Err(format!("this deletes update #{id}; run again with --yes"));
            }
            store.delete_update(id).map_err(|e| e.to_string())?;
            println!("Deleted update #{id}.");
            Ok(())
        }
        Command::Feed { project, limit, json } => {
            let project = resolve_existing(&store, project.as_deref())?;
            let feed = store.feed(project.id, limit, None).map_err(|e| e.to_string())?;
            let ids: Vec<i64> = feed.iter().map(|u| u.id).collect();
            let replies = store.replies_for_updates(&ids).map_err(|e| e.to_string())?;
            let todos = store.todos(project.id).map_err(|e| e.to_string())?;
            if json {
                let updates: Vec<_> = feed
                    .iter()
                    .map(|u| {
                        let mut v = update_json(u);
                        let rs: Vec<_> = replies
                            .iter()
                            .filter(|r| r.update_id == u.id)
                            .map(|r| json!({ "id": r.id, "author": r.author, "from_human": r.from_human, "body": r.body, "created_at": r.created_at, "read_at": r.read_at }))
                            .collect();
                        v["replies"] = json!(rs);
                        v
                    })
                    .collect();
                let todos_json: Vec<_> = todos.iter().map(todo_json).collect();
                println!("{}", json!({ "project": project_json(&project), "todos": todos_json, "updates": updates }));
                return Ok(());
            }
            if let Some(brief) = &project.brief {
                println!("Brief:\n{brief}\n");
            }
            if !todos.is_empty() {
                let open_count = todos.iter().filter(|t| !t.done).count();
                println!("To-dos ({open_count} open / {} total):", todos.len());
                for t in &todos {
                    let mark = if t.done { "x" } else { " " };
                    println!("  [{mark}] {} (#{})", t.title, t.id);
                }
                println!();
            }
            if feed.is_empty() {
                println!("{} has no updates yet.", project.name);
            }
            let now = now_ms();
            for (i, u) in feed.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                let branch = u.branch.as_deref().map(|b| format!(" · {b}")).unwrap_or_default();
                let diff = u.diff_stat.as_deref().map(|d| format!(" · {d}")).unwrap_or_default();
                println!("── {} · {}{branch}{diff} (#{})", u.agent, ago(u.created_at, now), u.id);
                println!("{}", u.body);
                if !u.media.is_empty() {
                    for m in &u.media {
                        println!("[media] {}", m.path.display());
                    }
                } else if let Some(shot) = &u.screenshot {
                    println!("[media] {}", shot.display());
                }
                for r in replies.iter().filter(|r| r.update_id == u.id) {
                    println!("  ↳ {} ({}): {}", r.author, ago(r.created_at, now), r.body);
                }
            }
            Ok(())
        }
        Command::Reply { id, message, agent, from_agent, json } => {
            let body = read_body(message)?;
            let from_human = !from_agent;
            let author = match agent {
                Some(a) => agent_display_name(&a),
                None if from_human => "You".to_string(),
                None => agent_display_name(&default_agent()),
            };
            let reply = store.add_reply(id, &author, from_human, &body).map_err(|e| e.to_string())?;
            if json {
                println!(
                    "{}",
                    json!({
                        "id": reply.id,
                        "update_id": reply.update_id,
                        "project_id": reply.project_id,
                        "author": reply.author,
                        "from_human": reply.from_human,
                        "body": reply.body,
                        "created_at": reply.created_at,
                        "read_at": reply.read_at,
                    })
                );
            } else {
                println!("Replied to update #{id} as {}.", reply.author);
            }
            Ok(())
        }
        Command::Replies { project, unread, mark_read, json } => {
            let target_project = match project.as_deref() {
                Some(name) => Some(find(&store, name)?),
                None if !unread => resolve_existing(&store, None).ok(),
                None => None,
            };
            let mut replies = if unread {
                store.unread_human_replies(target_project.as_ref().map(|p| p.id)).map_err(|e| e.to_string())?
            } else if let Some(p) = &target_project {
                let feed = store.feed(p.id, 30, None).map_err(|e| e.to_string())?;
                let ids: Vec<i64> = feed.iter().map(|u| u.id).collect();
                store.replies_for_updates(&ids).map_err(|e| e.to_string())?
            } else {
                store.unread_human_replies(None).map_err(|e| e.to_string())?
            };
            let names = store.project_names().map_err(|e| e.to_string())?;
            if mark_read && !replies.is_empty() {
                let ids: Vec<i64> = replies.iter().filter(|r| r.from_human && r.read_at.is_none()).map(|r| r.id).collect();
                if !ids.is_empty() {
                    store.mark_replies_read(&ids).map_err(|e| e.to_string())?;
                    let read_at = now_ms();
                    for r in &mut replies {
                        if r.from_human && r.read_at.is_none() {
                            r.read_at = Some(read_at);
                        }
                    }
                }
            }
            if json {
                let list: Vec<_> = replies
                    .iter()
                    .map(|r| {
                        json!({
                            "id": r.id,
                            "update_id": r.update_id,
                            "project_id": r.project_id,
                            "project": names.get(&r.project_id).cloned().unwrap_or_default(),
                            "author": r.author,
                            "from_human": r.from_human,
                            "body": r.body,
                            "created_at": r.created_at,
                            "read_at": r.read_at,
                        })
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(list));
                return Ok(());
            }
            if replies.is_empty() {
                println!("{}", if unread { "No unread replies." } else { "No replies yet." });
                return Ok(());
            }
            let now = now_ms();
            for r in &replies {
                let pname = names.get(&r.project_id).map(String::as_str).unwrap_or("Unknown");
                let badge = if r.from_human && r.read_at.is_none() { " [unread]" } else { "" };
                println!(
                    "#{0} ({pname} · update #{1}){badge} — {2} ({3}): {4}",
                    r.id,
                    r.update_id,
                    r.author,
                    ago(r.created_at, now),
                    r.body
                );
            }
            Ok(())
        }
        Command::Brief { text, project, clear, json } => {
            let p = resolve_existing(&store, project.as_deref())?;
            if clear {
                store.set_project_brief(p.id, None).map_err(|e| e.to_string())?;
            } else if !text.is_empty() {
                let body = read_body(text)?;
                store.set_project_brief(p.id, Some(&body)).map_err(|e| e.to_string())?;
            }
            let updated = store.project(p.id).map_err(|e| e.to_string())?.ok_or("project vanished")?;
            if json {
                println!("{}", json!({ "project": project_json(&updated) }));
            } else if let Some(brief) = &updated.brief {
                println!("{brief}");
            } else {
                println!("{} has no pinned brief.", updated.name);
            }
            Ok(())
        }
        Command::Todo { action, project, open, json } => run_todo(&store, action, project, open, json),
        Command::Notebook { action } => {
            let author = agent_from_env().unwrap_or_else(|| feeder_core::HUMAN_AUTHOR.to_string());
            notebook::run(&store, action, &author)
        }
        Command::Flag { update: Some(id), agent, json, .. } => {
            let author = agent.as_deref().map_or_else(|| feeder_core::HUMAN_AUTHOR.to_string(), agent_display_name);
            let flag = store.flag_update_for_notebook(id, &author).map_err(|e| e.to_string())?;
            gardener::print_flag(&flag, json);
            Ok(())
        }
        Command::Flag { note, kind, page, suggest, suggest_file, project, shared, agent, json, .. } => {
            let kind = kind.as_deref().unwrap_or("missing").parse().map_err(|e: feeder_core::Error| e.to_string())?;
            let project_id = if shared {
                None
            } else {
                match project {
                    Some(name) => {
                        Some(store.find_project(&name).map_err(|e| e.to_string())?.ok_or(format!("no project called {name}"))?.id)
                    }
                    None => std::env::current_dir()
                        .ok()
                        .and_then(|here| store.find_detected(&detect_project(&here)).ok().flatten())
                        .map(|p| p.id),
                }
            };
            let suggestion = match suggest_file {
                Some(path) => Some(std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?),
                None => suggest,
            };
            let author = agent
                .as_deref()
                .map_or_else(|| agent_from_env().unwrap_or_else(|| feeder_core::HUMAN_AUTHOR.to_string()), agent_display_name);
            let flag = feeder_core::NewFlag { kind, page_id: page, project_id, note: note.join(" "), suggestion };
            gardener::flag(&store, flag, &author, json)
        }
        Command::Flags { all, dismiss, reason, json } => gardener::flags(&store, all, dismiss, reason, json),
        Command::Gardener { action } => gardener::run(&store, action),
        Command::Pulse { status, project, agent, branch, ttl, clear, json } => {
            let dir = std::env::current_dir().map_err(|e| e.to_string())?;
            let explicit_project = project.is_some();
            let project = match project {
                Some(name) => store.project_named(&name, None),
                None => store.project_at(&detect_project(&dir)),
            }
            .map_err(|e| e.to_string())?;
            let agent = agent_display_name(&agent.unwrap_or_else(default_agent));
            if clear {
                store.clear_pulse(project.id, &agent).map_err(|e| e.to_string())?;
                if json {
                    println!("{}", json!({ "cleared": true, "project": project.name, "agent": agent }));
                } else {
                    println!("Cleared pulse for {} ({agent}).", project.name);
                }
                return Ok(());
            }
            let status_text = if status.is_empty() { "Working".to_string() } else { status.join(" ") };
            let git_dir = if !explicit_project { Some(dir) } else { project.path.as_deref().map(PathBuf::from).filter(|p| p.exists()) };
            let branch = branch.or_else(|| git_dir.as_deref().and_then(|d| detect_git_meta(d).branch));
            let ttl_ms = i64::try_from(ttl.saturating_mul(1000)).unwrap_or(120_000);
            let pulse = store.set_pulse(project.id, &agent, branch.as_deref(), &status_text, ttl_ms).map_err(|e| e.to_string())?;
            if json {
                println!(
                    "{}",
                    json!({
                        "project": project.name,
                        "agent": pulse.agent,
                        "branch": pulse.branch,
                        "status": pulse.status,
                        "updated_at": pulse.updated_at,
                        "expires_at": pulse.expires_at,
                    })
                );
            } else {
                println!("Pulse set on {} ({}): {}", project.name, pulse.agent, pulse.status);
            }
            Ok(())
        }
        Command::InstallHook { dir, remove } => {
            let dir = match dir {
                Some(d) => absolute(&d),
                None => std::env::current_dir().map_err(|e| e.to_string())?,
            };
            if remove {
                if remove_post_commit_hook(&dir).map_err(|e| e.to_string())? {
                    println!("Removed Feeder post-commit hook.");
                } else {
                    println!("No Feeder post-commit hook found.");
                }
            } else {
                let bin = setup::feeder_binary();
                let hook = install_post_commit_hook(&dir, &bin).map_err(|e| e.to_string())?;
                let _ = store.project_at(&detect_project(&dir)).map_err(|e| e.to_string())?;
                println!("Installed post-commit hook at {}", hook.display());
            }
            Ok(())
        }
        Command::HookPostCommit { dir } => {
            let dir = match dir {
                Some(d) => absolute(&d),
                None => std::env::current_dir().map_err(|e| e.to_string())?,
            };
            let detected = detect_project(&dir);
            let Some(project) = store.find_detected(&detected).map_err(|e| e.to_string())? else {
                return Ok(());
            };
            let Some((subject, meta)) = latest_commit_summary(&dir) else {
                return Ok(());
            };
            if let Some(sha) = meta.commit_sha.as_deref()
                && store.has_commit(project.id, sha).map_err(|e| e.to_string())?
            {
                return Ok(());
            }
            let sha_tag = meta.commit_sha.as_deref().map(|s| format!(" `{s}`")).unwrap_or_default();
            let body = format!("Committed{sha_tag}: {subject}");
            let agent = agent_from_env().unwrap_or_else(|| "Git".to_string());
            let _ = store.post(
                project.id,
                NewUpdate {
                    agent,
                    body,
                    screenshot: None,
                    screenshots: Vec::new(),
                    branch: meta.branch,
                    diff_stat: meta.diff_stat,
                    commit_sha: meta.commit_sha,
                },
            );
            Ok(())
        }
        Command::Projects { archived, all, json } => {
            let projects: Vec<_> = store
                .projects()
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|p| all || if archived { p.project.archived } else { !p.project.archived })
                .collect();
            if json {
                let list: Vec<_> = projects
                    .iter()
                    .map(|p| {
                        let mut v = project_json(&p.project);
                        v["updates"] = json!(p.updates);
                        v["last_at"] = json!(p.last_at);
                        v["last_agent"] = json!(p.last_agent);
                        v["needs_you"] = json!(p.needs_you);
                        v["unread_replies"] = json!(p.unread_replies);
                        v["open_todos"] = json!(p.open_todos);
                        v["active_pulse"] = p
                            .active_pulse
                            .as_ref()
                            .map(|pulse| {
                                json!({
                                    "agent": pulse.agent,
                                    "branch": pulse.branch,
                                    "status": pulse.status,
                                    "updated_at": pulse.updated_at,
                                    "expires_at": pulse.expires_at,
                                })
                            })
                            .unwrap_or(serde_json::Value::Null);
                        v
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(list));
                return Ok(());
            }
            if projects.is_empty() {
                if archived {
                    println!("No archived projects.");
                } else {
                    println!("No projects yet. Post one with: feeder post \"what changed\"");
                }
            }
            let now = now_ms();
            for p in projects {
                let last = match (p.last_at, &p.last_agent) {
                    (Some(at), Some(agent)) => format!("{} · {agent}", ago(at, now)),
                    _ => "no updates".into(),
                };
                let folder = p.project.folder().filter(|_| p.project.renamed()).map(|f| format!(" ({f})")).unwrap_or_default();
                let pin = if p.project.pinned { " [pinned]" } else { "" };
                let needs = if p.needs_you { " [needs you]" } else { "" };
                let pulse = p.active_pulse.as_ref().map(|_| " [working]").unwrap_or("");
                let todos_tag = if p.open_todos > 0 {
                    format!(" [{} to-do{}]", p.open_todos, if p.open_todos == 1 { "" } else { "s" })
                } else {
                    String::new()
                };
                let tag = if all && p.project.archived { " [archived]" } else { "" };
                let count = if p.updates == 1 { "1 update ".to_string() } else { format!("{} updates", p.updates) };
                println!("{:<36} {count:>11}   {last}", format!("{}{folder}{pin}{needs}{pulse}{todos_tag}{tag}", p.project.name));
            }
            Ok(())
        }
        Command::Add { dir, name } => {
            let dir = match dir {
                Some(d) => absolute(&d),
                None => std::env::current_dir().map_err(|e| e.to_string())?,
            };
            if !dir.is_dir() {
                return Err(format!("{} is not a folder", dir.display()));
            }
            let p = store.project_at(&detect_project(&dir)).map_err(|e| e.to_string())?;
            if let Some(name) = name {
                store.rename_project(p.id, &name).map_err(|e| e.to_string())?;
            }
            let p = store.project(p.id).map_err(|e| e.to_string())?.ok_or("project vanished")?;
            println!("{} → {}", p.name, p.path.as_deref().map(|p| p.display().to_string()).unwrap_or_default());
            Ok(())
        }
        Command::Rename { project, name } => {
            let p = find(&store, &project)?;
            store.rename_project(p.id, &name).map_err(|e| e.to_string())?;
            let renamed = store.project(p.id).map_err(|e| e.to_string())?.ok_or("project vanished")?;
            match renamed.folder() {
                Some(folder) => println!("Renamed {} to {} (folder: {folder}).", p.name, renamed.name),
                None => println!("Renamed {} to {}.", p.name, renamed.name),
            }
            Ok(())
        }
        Command::Archive { project } => {
            let p = resolve_existing(&store, project.as_deref())?;
            store.set_archived(p.id, true).map_err(|e| e.to_string())?;
            println!("Archived {}.", p.name);
            Ok(())
        }
        Command::Unarchive { project } => {
            let p = resolve_existing(&store, project.as_deref())?;
            store.set_archived(p.id, false).map_err(|e| e.to_string())?;
            println!("Unarchived {}.", p.name);
            Ok(())
        }
        Command::Pin { project } => {
            let p = resolve_existing(&store, project.as_deref())?;
            store.set_pinned(p.id, true).map_err(|e| e.to_string())?;
            println!("Pinned {}.", p.name);
            Ok(())
        }
        Command::Unpin { project } => {
            let p = resolve_existing(&store, project.as_deref())?;
            store.set_pinned(p.id, false).map_err(|e| e.to_string())?;
            println!("Unpinned {}.", p.name);
            Ok(())
        }
        Command::Search { query, project, archived, limit, json } => {
            let pid = match project.as_deref() {
                Some(name) => Some(find(&store, name)?.id),
                None => None,
            };
            let hits = store.search_updates(&query, pid, archived || pid.is_some(), limit).map_err(|e| e.to_string())?;
            let names: std::collections::HashMap<i64, String> =
                store.projects().map_err(|e| e.to_string())?.into_iter().map(|s| (s.project.id, s.project.name)).collect();
            if json {
                let list: Vec<_> = hits
                    .iter()
                    .map(|u| {
                        let mut v = update_json(u);
                        v["project"] = json!(names.get(&u.project_id).cloned().unwrap_or_default());
                        v
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(list));
                return Ok(());
            }
            if hits.is_empty() {
                println!("No matches for \"{query}\".");
                return Ok(());
            }
            let now = now_ms();
            for (i, u) in hits.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                let proj = names.get(&u.project_id).map(String::as_str).unwrap_or("Unknown");
                let branch = u.branch.as_deref().map(|b| format!(" · {b}")).unwrap_or_default();
                println!("── {proj} · {} · {}{branch} (#{})", u.agent, ago(u.created_at, now), u.id);
                println!("{}", u.body);
                if !u.media.is_empty() {
                    for m in &u.media {
                        println!("[media] {}", m.path.display());
                    }
                } else if let Some(shot) = &u.screenshot {
                    println!("[media] {}", shot.display());
                }
            }
            Ok(())
        }
        Command::Seen { project, all } => {
            if all {
                store.mark_all_seen().map_err(|e| e.to_string())?;
                println!("Marked all active projects as read.");
            } else {
                let p = resolve_existing(&store, project.as_deref())?;
                store.mark_seen(p.id).map_err(|e| e.to_string())?;
                println!("Marked {} as read.", p.name);
            }
            Ok(())
        }
        Command::Dismiss { project, id } => {
            if let Some(update_id) = id {
                store.dismiss_attention(update_id).map_err(|e| e.to_string())?;
                println!("Dismissed update #{update_id}.");
            } else {
                let p = resolve_existing(&store, project.as_deref())?;
                if store.dismiss_project_attention(p.id).map_err(|e| e.to_string())? {
                    println!("Dismissed {}'s latest update.", p.name);
                } else {
                    println!("{} has no updates to dismiss.", p.name);
                }
            }
            Ok(())
        }
        Command::Remove { project, yes } => {
            let p = find(&store, &project)?;
            if !yes {
                return Err(format!("this deletes {} and its whole feed; run again with --yes", p.name));
            }
            store.remove_project(p.id).map_err(|e| e.to_string())?;
            println!("Removed {}.", p.name);
            Ok(())
        }
        Command::Mcp { .. } | Command::Serve { .. } | Command::Where | Command::Guide | Command::Setup { .. } => {
            unreachable!("handled in main")
        }
    }
}

fn read_body(message: Vec<String>) -> Result<String, String> {
    if message.is_empty() || message == ["-"] {
        if std::io::stdin().is_terminal() {
            return Err("give the update as an argument or on stdin".into());
        }
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
        Ok(s)
    } else {
        Ok(message.join(" "))
    }
}

fn default_agent() -> String {
    agent_from_env().unwrap_or_else(|| "Unknown agent".into())
}

fn run_setup(names: Vec<String>, all: bool, print: bool, remove: bool) -> Result<(), String> {
    use setup::{Outcome, Tool};
    let bin = setup::feeder_binary();
    let mut tools = Vec::new();
    for n in &names {
        tools.push(Tool::parse(n).ok_or_else(|| format!("unknown tool `{n}`; run `feeder setup` to list them"))?);
    }
    if all {
        tools.extend(Tool::ALL.into_iter().filter(|t| remove || setup::detected(*t)));
    }
    tools.dedup();
    if tools.is_empty() {
        println!("Feeder's MCP server is local: each tool runs `{} mcp` on this Mac.\n", bin.display());
        for t in Tool::ALL {
            let found = if setup::detected(t) { "found" } else { "not found" };
            println!("  {:<16} {:<22} {found}", t.id(), t.name());
        }
        println!(
            "\nConnect some:  feeder setup codex cursor\nConnect all:   feeder setup --all\nJust show it:  feeder setup --print opencode\nUndo all:      feeder setup --remove --all"
        );
        return Ok(());
    }
    if print {
        for t in tools {
            println!("── {}\n{}", t.name(), setup::snippet(t, &bin));
        }
        return Ok(());
    }
    for t in tools {
        println!("{}", t.name());
        let results = if remove { setup::disconnect(t) } else { setup::connect(t, &bin) };
        for (place, outcome) in results {
            let what = match outcome {
                Outcome::Added => "connected".to_string(),
                Outcome::Updated => "updated".to_string(),
                Outcome::Removed => "disconnected".to_string(),
                Outcome::Already if remove => "not connected".to_string(),
                Outcome::Already => "already connected".to_string(),
                Outcome::Skipped(why) => format!("skipped: {why}"),
            };
            println!("  {what:<20} {place}");
        }
    }
    if remove {
        if all {
            for s in setup::remove_skills() {
                println!("  removed skill        {}", s.display());
            }
        }
        println!("\nRestart the tools to stop them starting Feeder.");
        return Ok(());
    }
    let skills = setup::install_skills();
    if !skills.is_empty() {
        println!("Skills (feeder-update, feeder-notebook)");
        for s in skills {
            println!("  installed            {}", s.display());
        }
    }
    println!("\nRestart the tools (or start a new session) to pick it up.");
    Ok(())
}

fn absolute(p: &std::path::Path) -> PathBuf {
    if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().map(|d| d.join(p)).unwrap_or(p.to_path_buf()) }
}

fn find(store: &Store, name: &str) -> Result<Project, String> {
    store.find_project(name).map_err(|e| e.to_string())?.ok_or_else(|| format!("no project called {name}"))
}

fn resolve_existing(store: &Store, name: Option<&str>) -> Result<Project, String> {
    match name {
        Some(n) => find(store, n),
        None => {
            let here = detect_project(&std::env::current_dir().map_err(|e| e.to_string())?);
            store.find_detected(&here).map_err(|e| e.to_string())?.ok_or_else(|| format!("{} has no feed yet; post to it first", here.name))
        }
    }
}

pub(crate) fn project_json(p: &Project) -> serde_json::Value {
    json!({
        "id": p.id,
        "slug": p.slug,
        "name": p.name,
        "folder": p.folder(),
        "path": p.path,
        "archived": p.archived,
        "pinned": p.pinned,
        "brief": p.brief,
    })
}

pub(crate) fn update_json(u: &FeedUpdate) -> serde_json::Value {
    let media_kind = u.media_kind().map(|k| match k {
        feeder_core::MediaKind::Image => "image",
        feeder_core::MediaKind::Video => "video",
    });
    let media: Vec<_> = u
        .media
        .iter()
        .map(|m| {
            let kind = m.kind().map(|k| match k {
                feeder_core::MediaKind::Image => "image",
                feeder_core::MediaKind::Video => "video",
            });
            json!({
                "id": m.id,
                "position": m.position,
                "path": m.path,
                "kind": kind,
                "w": m.width,
                "h": m.height,
                "caption": m.caption,
            })
        })
        .collect();
    json!({
        "id": u.id,
        "agent": u.agent,
        "body": u.body,
        "screenshot": u.screenshot,
        "media": media,
        "media_kind": media_kind,
        "media_w": u.media_w,
        "media_h": u.media_h,
        "branch": u.branch,
        "diff_stat": u.diff_stat,
        "commit_sha": u.commit_sha,
        "created_at": u.created_at,
        "attention_dismissed_at": u.attention_dismissed_at,
    })
}

pub(crate) fn todo_json(t: &TodoItem) -> serde_json::Value {
    json!({
        "id": t.id,
        "project_id": t.project_id,
        "title": t.title,
        "done": t.done,
        "position": t.position,
        "created_at": t.created_at,
        "completed_at": t.completed_at,
    })
}

fn run_todo(store: &Store, action: Option<TodoCmd>, project: Option<String>, open: bool, json: bool) -> Result<(), String> {
    let cmd = action.unwrap_or(TodoCmd::List { project, open, json });
    match cmd {
        TodoCmd::List { project, open, json } => {
            let p = resolve_existing(store, project.as_deref())?;
            let mut items = store.todos(p.id).map_err(|e| e.to_string())?;
            if open {
                items.retain(|t| !t.done);
            }
            if json {
                let list: Vec<_> = items.iter().map(todo_json).collect();
                println!("{}", json!({ "project": project_json(&p), "todos": list }));
                return Ok(());
            }
            if items.is_empty() {
                println!("{} has no{} to-dos.", p.name, if open { " open" } else { "" });
                return Ok(());
            }
            for t in &items {
                let mark = if t.done { "x" } else { " " };
                println!("[{mark}] {} (#{})", t.title, t.id);
            }
            Ok(())
        }
        TodoCmd::Add { title, project, json } => {
            let title = if title.is_empty() || title == ["-"] {
                read_body(title).map_err(|_| "give the to-do title as an argument or on stdin".to_string())?
            } else {
                title.join(" ")
            };
            let dir = std::env::current_dir().map_err(|e| e.to_string())?;
            let p = match project {
                Some(name) => store.project_named(&name, None),
                None => store.project_at(&detect_project(&dir)),
            }
            .map_err(|e| e.to_string())?;
            let item = store.add_todo(p.id, &title).map_err(|e| e.to_string())?;
            if json {
                println!("{}", json!({ "project": project_json(&p), "todo": todo_json(&item) }));
            } else {
                println!("Added to-do #{} to {}: {}", item.id, p.name, item.title);
            }
            Ok(())
        }
        TodoCmd::Done { id, reopen, json } => {
            let item = store.set_todo_done(id, !reopen).map_err(|e| e.to_string())?;
            if json {
                println!("{}", todo_json(&item));
            } else {
                let state = if item.done { "Done" } else { "Reopened" };
                println!("{state} #{}: {}", item.id, item.title);
            }
            Ok(())
        }
        TodoCmd::Edit { id, title, json } => {
            let title = if title.is_empty() || title == ["-"] {
                read_body(title).map_err(|_| "give the new to-do title as an argument or on stdin".to_string())?
            } else {
                title.join(" ")
            };
            let item = store.rename_todo(id, &title).map_err(|e| e.to_string())?;
            if json {
                println!("{}", todo_json(&item));
            } else {
                println!("Updated to-do #{}: {}", item.id, item.title);
            }
            Ok(())
        }
        TodoCmd::Rm { id, json } => {
            store.delete_todo(id).map_err(|e| e.to_string())?;
            if json {
                println!("{}", json!({ "deleted": id }));
            } else {
                println!("Deleted to-do #{id}.");
            }
            Ok(())
        }
        TodoCmd::Clear { project, all, json } => {
            let p = resolve_existing(store, project.as_deref())?;
            let removed = if all {
                store.clear_project_todos(p.id).map_err(|e| e.to_string())?
            } else {
                store.clear_completed_todos(p.id).map_err(|e| e.to_string())?
            };
            if json {
                println!("{}", json!({ "project": project_json(&p), "removed": removed, "all": all }));
            } else {
                println!(
                    "Cleared {removed} {} to-do{} in {}.",
                    if all { "total" } else { "completed" },
                    if removed == 1 { "" } else { "s" },
                    p.name
                );
            }
            Ok(())
        }
    }
}
