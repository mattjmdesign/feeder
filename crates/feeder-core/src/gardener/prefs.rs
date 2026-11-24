use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Backend, profile};
use crate::{Error, Result};

const FILE: &str = "curator.json";
pub const NICKNAME_CHARS: usize = 32;
const MAX_MODEL_CHARS: usize = 128;
const MAX_EFFORT_CHARS: usize = 32;
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
const CODEX_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const LIST_OUTPUT_BYTES: u64 = 1 << 20;
const OPENCODE_CACHE_MAX_BYTES: u64 = 32 * 1024 * 1024;

const OPENCODE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
const ANTIGRAVITY_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Provider {
    pub enabled: bool,
    pub nickname: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Prefs {
    pub default: Option<String>,
    pub providers: BTreeMap<String, Provider>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunConfig {
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl RunConfig {
    pub fn validate(&self) -> Result<()> {
        if let Some(model) = &self.model {
            validate_model(model)?;
        }
        if let Some(effort) = &self.effort {
            validate_effort(effort)?;
        }
        Ok(())
    }
}

fn validate_model(model: &str) -> Result<()> {
    if model.is_empty()
        || model.chars().count() > MAX_MODEL_CHARS
        || model.starts_with('-')
        || model.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(Error::Invalid("Invalid Curator model setting. Choose a model again in Settings → Curator.".into()));
    }
    Ok(())
}

fn validate_effort(effort: &str) -> Result<()> {
    if effort.is_empty()
        || effort.len() > MAX_EFFORT_CHARS
        || effort.starts_with('-')
        || !effort.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::Invalid("Invalid Curator thinking level. Choose a level again in Settings → Curator.".into()));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    pub efforts: Vec<String>,
}

impl Prefs {
    pub fn load(dir: &Path) -> Prefs {
        std::fs::read_to_string(dir.join(FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.tmp-{}-{:08x}", std::process::id(), fastrand::u32(..)));
        let data = serde_json::to_string_pretty(self).expect("curator prefs serialize");
        let written = (|| -> io::Result<()> {
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            file.write_all(data.as_bytes())?;
            file.sync_all()
        })();
        if let Err(error) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        std::fs::rename(&tmp, dir.join(FILE)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    pub fn get(&self, backend: Backend) -> Provider {
        self.providers.get(backend.as_str()).cloned().unwrap_or_default()
    }

    pub fn edit(&mut self, backend: Backend, f: impl FnOnce(&mut Provider)) {
        f(self.providers.entry(backend.as_str().to_string()).or_default());
    }

    pub fn is_enabled(&self, backend: Backend) -> bool {
        self.providers.get(backend.as_str()).is_some_and(|p| p.enabled)
    }

    pub fn set_enabled(&mut self, backend: Backend, on: bool) {
        self.edit(backend, |p| p.enabled = on);
        if on && self.default_backend().is_none() {
            self.default = Some(backend.as_str().into());
        }
        if !on && self.default.as_deref() == Some(backend.as_str()) {
            self.default = Backend::ALL.into_iter().find(|b| self.is_enabled(*b)).map(|b| b.as_str().into());
        }
    }

    pub fn enabled(&self) -> Vec<Backend> {
        Backend::ALL.into_iter().filter(|b| self.is_enabled(*b)).collect()
    }

    pub fn default_backend(&self) -> Option<Backend> {
        self.default.as_deref().and_then(|d| d.parse().ok()).filter(|b| self.is_enabled(*b))
    }

    pub fn name(&self, backend: Backend) -> String {
        let nick = self.get(backend).nickname;
        if nick.trim().is_empty() { backend.label().to_string() } else { nick.trim().to_string() }
    }

    pub fn set_nickname(&mut self, backend: Backend, nickname: &str) {
        let clean: String = nickname.chars().filter(|c| !c.is_control()).take(NICKNAME_CHARS).collect();
        self.edit(backend, |p| p.nickname = clean.trim().to_string());
    }

    pub fn run(&self, backend: Backend) -> RunConfig {
        let p = self.get(backend);
        RunConfig {
            model: p.model.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            effort: p.effort.map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()),
        }
    }
}

pub fn effort_label(effort: &str) -> String {
    match effort {
        "xhigh" => "Extra High".into(),
        "minimal" => "Minimal".into(),
        other => {
            let mut chars = other.chars();
            chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    }
}

impl Backend {
    pub fn models(self, data: &Path) -> Result<Vec<ModelInfo>> {
        match self {
            Backend::ClaudeCode => {
                let efforts: Vec<String> = ["low", "medium", "high", "xhigh", "max"].map(String::from).to_vec();
                Ok([("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku"), ("fable", "Fable")]
                    .into_iter()
                    .map(|(id, label)| ModelInfo { id: id.into(), label: label.into(), efforts: efforts.clone() })
                    .collect())
            }
            Backend::Codex => codex_models(data),
            Backend::OpenCode => {
                Ok(parse_opencode_with_cache(&self.list(data, &["models", "--standalone"])?, opencode_models_cache().as_ref()))
            }
            Backend::Antigravity => Ok(parse_antigravity(&self.list(data, &["models"])?)),
        }
    }

    fn list(self, data: &Path, args: &[&str]) -> Result<String> {
        let bin = self.binary().ok_or_else(|| Error::NotFound(format!("{} isn't installed", self.label())))?;
        let mut cmd = profile::command(self, &bin, data)?;
        cmd.args(args);
        if self == Self::Antigravity {
            profile::add_antigravity_gemini_dir(&mut cmd, data)?;
        }
        let (status, text) = read_command_stdout(&mut cmd, LIST_TIMEOUT).map_err(|e| {
            if e.kind() == io::ErrorKind::TimedOut {
                Error::Invalid(format!("{} took too long to list its models", self.label()))
            } else {
                Error::Io(e)
            }
        })?;
        if !status.success() {
            return Err(Error::Invalid(format!("{} couldn't list its models. Is it signed in?", self.label())));
        }
        Ok(text)
    }
}

fn read_command_stdout(cmd: &mut Command, timeout: Duration) -> io::Result<(std::process::ExitStatus, String)> {
    let mut child = cmd.process_group(0).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    let stdout = child.stdout.take();
    std::thread::scope(|scope| {
        let reader = scope.spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = stdout {
                let _ = pipe.by_ref().take(LIST_OUTPUT_BYTES).read_to_string(&mut text);
                let _ = io::copy(&mut pipe, &mut io::sink());
            }
            text
        });
        let start = Instant::now();
        let exited = loop {
            match crate::process::has_exited(child.id()) {
                Ok(true) => break Ok(()),
                Ok(false) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(25)),
                Ok(false) => break Err(io::Error::new(io::ErrorKind::TimedOut, "model list timed out")),
                Err(error) => break Err(error),
            }
        };
        let status = crate::process::terminate_group(&mut child);
        let text = reader.join().unwrap_or_default();
        exited?;
        Ok((status?, text))
    })
}

fn codex_models(data: &Path) -> Result<Vec<ModelInfo>> {
    if let Some(path) = dirs::home_dir().map(|h| h.join(".codex").join("models_cache.json"))
        && let Ok(text) = std::fs::read_to_string(&path)
    {
        let models = parse_codex(&text);
        if !models.is_empty() {
            return Ok(models);
        }
    }
    codex_models_rpc(data)
}

fn codex_models_rpc(data: &Path) -> Result<Vec<ModelInfo>> {
    let bin = Backend::Codex.binary().ok_or_else(|| Error::NotFound("Codex isn't installed".into()))?;
    let result =
        profile::codex_rpc(&bin, data, "model/list", CODEX_RPC_TIMEOUT, 1024 * 1024, "feeder-codex-models").map_err(|e| match e {
            Error::Io(io_err) if io_err.kind() == io::ErrorKind::TimedOut => {
                Error::Invalid("Codex took too long to list its models".into())
            }
            Error::Invalid(_) => Error::Invalid("Codex couldn't list its models. Is it signed in?".into()),
            other => other,
        })?;
    let models = parse_codex_rpc(&result);
    if models.is_empty() { Err(Error::NotFound("Codex hasn't listed its models yet. Run Codex once.".into())) } else { Ok(models) }
}

fn parse_codex(text: &str) -> Vec<ModelInfo> {
    let Ok(value) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    value["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"].as_str() != Some("hide"))
        .filter_map(|m| {
            let id = m["slug"].as_str()?.to_string();
            let label = m["display_name"].as_str().unwrap_or(&id).to_string();
            let efforts = m["supported_reasoning_levels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l["effort"].as_str().map(String::from))
                .collect();
            Some(ModelInfo { id, label, efforts })
        })
        .collect()
}

fn parse_codex_rpc(result: &Value) -> Vec<ModelInfo> {
    result["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["hidden"].as_bool() != Some(true))
        .filter_map(|m| {
            let id = m["id"].as_str().or_else(|| m["model"].as_str())?.to_string();
            let label = m["displayName"].as_str().unwrap_or(&id).to_string();
            let efforts = m["supportedReasoningEfforts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l["reasoningEffort"].as_str().map(String::from))
                .collect();
            Some(ModelInfo { id, label, efforts })
        })
        .collect()
}

fn opencode_models_cache() -> Option<Value> {
    let path = dirs::home_dir()?.join(".cache").join("opencode").join("models.json");
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > OPENCODE_CACHE_MAX_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn opencode_efforts_from_cache(cache: &Value, id: &str) -> Option<Vec<String>> {
    let (provider, model_id) = id.split_once('/')?;
    let entry = cache.get(provider)?.get("models")?.get(model_id)?;
    if let Some(options) = entry.get("reasoning_options").and_then(Value::as_array) {
        if let Some(values) = options.iter().find(|opt| opt["type"].as_str() == Some("effort")).and_then(|opt| opt["values"].as_array()) {
            return Some(values.iter().filter_map(Value::as_str).map(String::from).collect());
        }
        return Some(Vec::new());
    }
    if entry.get("reasoning").and_then(Value::as_bool) == Some(false) {
        return Some(Vec::new());
    }
    None
}

#[cfg(test)]
fn parse_opencode(text: &str) -> Vec<ModelInfo> {
    parse_opencode_with_cache(text, None)
}

fn parse_opencode_with_cache(text: &str, cache: Option<&Value>) -> Vec<ModelInfo> {
    let fallback: Vec<String> = OPENCODE_EFFORTS.iter().map(|s| (*s).to_string()).collect();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.contains('/') && !l.contains(char::is_whitespace))
        .map(|l| {
            let efforts = cache.and_then(|c| opencode_efforts_from_cache(c, l)).unwrap_or_else(|| fallback.clone());
            ModelInfo { id: l.into(), label: l.into(), efforts }
        })
        .collect()
}

fn parse_antigravity(text: &str) -> Vec<ModelInfo> {
    struct Row<'a> {
        id: &'a str,
        label: &'a str,
        split: Option<(&'a str, &'static str)>,
    }
    let rows: Vec<Row<'_>> = text
        .lines()
        .filter_map(|line| {
            let (id, label) = line.split_once('\t')?;
            let (id, label) = (id.trim(), label.trim());
            let split = ANTIGRAVITY_LEVELS.iter().find_map(|level| id.strip_suffix(&format!("-{level}")).map(|base| (base, *level)));
            Some(Row { id, label, split })
        })
        .collect();
    let mut level_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &rows {
        if let Some((base, _)) = row.split {
            *level_counts.entry(base).or_default() += 1;
        }
    }
    let mut models: Vec<ModelInfo> = Vec::new();
    for row in rows {
        let grouped = row.split.filter(|(base, _)| level_counts.get(base).copied().unwrap_or(0) > 1);
        let (base, level) = grouped.map_or((row.id, None), |(b, l)| (b, Some(l)));
        let name = match (level, row.label.rsplit_once(" (")) {
            (Some(_), Some((name, _))) => name,
            _ => row.label,
        };
        match models.iter_mut().find(|m| m.id == base) {
            Some(m) => m.efforts.extend(level.map(String::from)),
            None => models.push(ModelInfo { id: base.into(), label: name.into(), efforts: level.map(String::from).into_iter().collect() }),
        }
    }
    for m in &mut models {
        m.efforts.sort_by_key(|e| ANTIGRAVITY_LEVELS.iter().position(|l| l == e));
        m.efforts.dedup();
    }
    models
}

pub(super) fn antigravity_model(run: &RunConfig) -> Option<String> {
    let model = run.model.as_deref()?;
    let already_leveled = ANTIGRAVITY_LEVELS.iter().any(|level| model.ends_with(&format!("-{level}")));
    Some(match run.effort.as_deref() {
        Some(effort) if !already_leveled && ANTIGRAVITY_LEVELS.contains(&effort) => format!("{model}-{effort}"),
        _ => model.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn nothing_is_on_until_turned_on_and_default_follows() {
        let mut p = Prefs::default();
        assert!(p.enabled().is_empty());
        assert_eq!(p.default_backend(), None);
        p.set_enabled(Backend::Codex, true);
        p.set_enabled(Backend::ClaudeCode, true);
        assert_eq!(p.default_backend(), Some(Backend::Codex));
        p.set_enabled(Backend::Codex, false);
        assert_eq!(p.default_backend(), Some(Backend::ClaudeCode));
        assert_eq!(p.enabled(), vec![Backend::ClaudeCode]);
    }

    #[test]
    fn nicknames_are_trimmed_capped_and_fall_back_to_the_cli_name() {
        let mut p = Prefs::default();
        assert_eq!(p.name(Backend::ClaudeCode), "Claude Code");
        p.set_nickname(Backend::ClaudeCode, "  Work\n");
        assert_eq!(p.name(Backend::ClaudeCode), "Work");
        p.set_nickname(Backend::ClaudeCode, &"x".repeat(80));
        assert_eq!(p.get(Backend::ClaudeCode).nickname.chars().count(), NICKNAME_CHARS);
    }

    #[test]
    fn antigravity_levels_group_into_one_model() {
        let text = "gemini-4-neon-high\tGemini 4 Neon (High)\ngemini-4-neon-low\tGemini 4 Neon (Low)\ngemini-pro-orchestrator-neon\tOrchestrator (Neon)\ngpt-oss-120b-medium\tGPT-OSS 120B (Medium)\n";
        let models = parse_antigravity(text);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "gemini-4-neon");
        assert_eq!(models[0].label, "Gemini 4 Neon");
        assert_eq!(models[0].efforts, vec!["low", "high"]);
        assert_eq!(models[1].label, "Orchestrator (Neon)");
        assert_eq!(models[2].id, "gpt-oss-120b-medium");
        assert_eq!(models[2].label, "GPT-OSS 120B (Medium)");
        assert!(models[2].efforts.is_empty());
        let run = RunConfig { model: Some("gemini-4-neon".into()), effort: Some("high".into()) };
        assert_eq!(antigravity_model(&run).as_deref(), Some("gemini-4-neon-high"));
        let already = RunConfig { model: Some("gemini-4-neon-high".into()), effort: Some("high".into()) };
        assert_eq!(antigravity_model(&already).as_deref(), Some("gemini-4-neon-high"));
        let unsupported = RunConfig { model: Some("gemini-4-neon".into()), effort: Some("ultra".into()) };
        assert_eq!(antigravity_model(&unsupported).as_deref(), Some("gemini-4-neon"));
    }

    #[test]
    fn codex_hides_hidden_models_and_keeps_levels() {
        let text = r#"{"models":[{"slug":"a","display_name":"A","visibility":"list","supported_reasoning_levels":[{"effort":"low"},{"effort":"ultra"}]},{"slug":"b","visibility":"hide"}]}"#;
        let models = parse_codex(text);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].efforts, vec!["low", "ultra"]);
        assert_eq!(effort_label("xhigh"), "Extra High");
        assert_eq!(effort_label("ultra"), "Ultra");
    }

    #[test]
    fn codex_rpc_model_list_parses_display_names_and_efforts() {
        let value = json!({
            "data": [
                {
                    "id": "gpt-6.2-sol",
                    "displayName": "GPT-6.2-Sol",
                    "supportedReasoningEfforts": [{ "reasoningEffort": "low" }, { "reasoningEffort": "high" }]
                },
                {
                    "id": "hidden-model",
                    "displayName": "Hidden",
                    "hidden": true
                }
            ]
        });
        let models = parse_codex_rpc(&value);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-6.2-sol");
        assert_eq!(models[0].label, "GPT-6.2-Sol");
        assert_eq!(models[0].efforts, vec!["low", "high"]);
    }

    #[test]
    fn opencode_models_expose_acp_thinking_levels() {
        let models = parse_opencode("openai/gpt-6-sol\nnot a model\nopencode/gemini-4-neon\n");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "openai/gpt-6-sol");
        assert_eq!(models[0].efforts, vec!["low", "medium", "high", "xhigh", "max"]);

        let cache = json!({
            "opencode-go": {
                "models": {
                    "deepseek-v4.1-flash": {
                        "reasoning": true,
                        "reasoning_options": [{ "type": "effort", "values": ["low", "high", "max"] }]
                    },
                    "plain-chat": { "reasoning": false }
                }
            }
        });
        let cached =
            parse_opencode_with_cache("opencode-go/deepseek-v4.1-flash\nopencode-go/plain-chat\nother/unknown-model\n", Some(&cache));
        assert_eq!(cached[0].efforts, vec!["low", "high", "max"]);
        assert!(cached[1].efforts.is_empty());
        assert_eq!(cached[2].efforts, vec!["low", "medium", "high", "xhigh", "max"]);
    }

    #[test]
    fn run_config_rejects_flag_injection_and_control_chars() {
        assert!(RunConfig { model: Some("openai/gpt-6-sol".into()), effort: Some("xhigh".into()) }.validate().is_ok());
        assert!(RunConfig { model: Some("--dangerously-skip-permissions".into()), effort: None }.validate().is_err());
        assert!(RunConfig { model: Some("sonnet --extra".into()), effort: None }.validate().is_err());
        assert!(RunConfig { model: Some("sonnet\n".into()), effort: None }.validate().is_err());
        assert!(RunConfig { model: None, effort: Some("--fast".into()) }.validate().is_err());
        assert!(RunConfig { model: None, effort: Some("high\"".into()) }.validate().is_err());
    }

    #[test]
    fn model_list_timeout_reaps_children_holding_stdout_open() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let start = Instant::now();
        let error = read_command_stdout(&mut cmd, Duration::from_millis(80)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(4));
    }
}
