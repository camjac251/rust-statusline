use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate, Utc};
use clap::Parser as _;
use owo_colors::OwoColorize;
use chrono::Timelike;
use serde::{Deserialize};
use serde_json::Value;
use std::{collections::HashSet, env, fs::File, io::{BufRead, BufReader, Read}, path::{Path, PathBuf}};
use gix::{self};

#[derive(Deserialize, Debug)]
struct HookModel {
    id: String,
    display_name: String,
}

#[derive(Deserialize, Debug)]
struct HookWorkspace {
    current_dir: String,
    project_dir: Option<String>,
}

#[derive(Deserialize, Debug)]
struct HookJson {
    session_id: String,
    transcript_path: String,
    cwd: Option<String>,
    model: HookModel,
    workspace: HookWorkspace,
    version: Option<String>,
}

#[derive(Deserialize, Debug)]
struct MessageUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct MessageObj { usage: Option<MessageUsage> }

#[derive(Deserialize, Debug)]
struct TranscriptLine { r#type: Option<String>, message: Option<MessageObj> }

#[derive(Deserialize, Debug)]
struct UsageLineMessage { usage: MessageUsage, model: Option<String> }

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum TimeFormatArg { Auto, H12, H24 }

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum LabelsArg { Short, Long }

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum GitArg { Minimal, Verbose }

#[derive(clap::Parser, Debug)]
struct Args {
    /// Force Claude data path(s), comma-separated. Defaults to ~/.config/claude and ~/.claude
    #[arg(long, env = "CLAUDE_CONFIG_DIR")]
    claude_config_dir: Option<String>,
    /// Emit JSON instead of colored text
    #[arg(long)]
    json: bool,
    /// Label verbosity for text output: short|long
    #[arg(long, value_enum, default_value_t = LabelsArg::Short)]
    labels: LabelsArg,
    /// Git segment style: minimal|verbose
    #[arg(long, value_enum, default_value_t = GitArg::Minimal)]
    git: GitArg,
    /// Time display: auto|12h|24h
    #[arg(long = "time", value_enum, default_value_t = TimeFormatArg::Auto)]
    time_fmt: TimeFormatArg,
    /// Show provider hint in header (hidden by default)
    #[arg(long)]
    show_provider: bool,
    /// Plan tier: pro|max5x|max20x (overrides env)
    #[arg(long, value_enum)]
    plan_tier: Option<PlanTierArg>,
    /// Plan max tokens per window (overrides tier/env)
    #[arg(long)]
    plan_max_tokens: Option<u64>,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum PlanTierArg { Pro, Max5x, Max20x }

fn claude_paths(override_env: Option<&str>) -> Vec<PathBuf> {
    let mut paths = vec![];
    if let Some(list) = override_env {
        let list = list.trim();
        if !list.is_empty() {
            for p in list.split(',') {
                let p = p.trim();
                if p.is_empty() { continue; }
                let pb = PathBuf::from(p);
                if pb.join("projects").is_dir() { paths.push(pb); }
            }
            if !paths.is_empty() { return paths; }
        }
    }
    let basedirs = directories::BaseDirs::new();
    let home = basedirs.as_ref().map(|b| b.home_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("~"));
    let xdg_config = basedirs.as_ref().map(|b| b.config_dir().to_path_buf()).unwrap_or_else(|| home.join(".config"));
    for base in [xdg_config.join("claude"), home.join(".claude")].into_iter() {
        if base.join("projects").is_dir() { paths.push(base); }
    }
    paths
}

fn deduce_provider_from_model(model_id: &str) -> &'static str {
    let m = model_id.to_lowercase();
    if m.contains('@') { return "vertex"; }
    if (m.contains("anthropic") && (m.contains(":") || m.contains("us."))) { return "bedrock"; }
    "anthropic"
}

fn read_stdin() -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    Ok(buf)
}

fn format_path(p: &str) -> String {
    if let Some(b) = directories::BaseDirs::new() {
        let home_s = b.home_dir().to_string_lossy();
        if p.starts_with(&*home_s) { return format!("~{}", &p[home_s.len()..]); }
    }
    p.to_owned()
}

fn format_currency(v: f64) -> String { format!("{v:.2}") }

fn parse_iso_date(s: &str) -> Option<NaiveDate> {
    let dt: DateTime<Utc> = DateTime::parse_from_rfc3339(s).ok()?.with_timezone(&Utc);
    // compare in local date, like ccusage tables typically show
    Some(dt.with_timezone(&Local).date_naive())
}

fn static_context_limit_lookup(model_id: &str) -> Option<u64> {
    let m = model_id.to_lowercase();
    // Known variants – currently all 200k; structure allows easy updates later
    if m.contains("opus-4-1") { return Some(200_000); }
    if m.contains("opus-4") { return Some(200_000); }
    if m.contains("sonnet-4") || m.contains("4-sonnet") { return Some(200_000); }
    if m.contains("3-7-sonnet") { return Some(200_000); }
    if m.contains("3-5-sonnet") { return Some(200_000); }
    if m.contains("3-5-haiku") { return Some(200_000); }
    None
}

fn context_limit_for_model(model_id: &str) -> u64 {
    // Allow explicit override when known
    if let Ok(override_limit) = env::var("CLAUDE_CONTEXT_LIMIT").and_then(|s| s.parse::<u64>().map_err(|_| std::env::VarError::NotPresent)) {
        return override_limit;
    }
    if let Some(v) = static_context_limit_lookup(model_id) { return v; }
    // Family fallback
    let m = model_id.to_lowercase();
    if m.contains("opus") { 200_000 }
    else if m.contains("sonnet") { 200_000 }
    else if m.contains("haiku") { 200_000 }
    else { 200_000 }
}

fn calc_context_from_transcript(transcript_path: &Path, model_id: &str) -> Option<(u64, u32)> {
    // Mimic ccusage: last assistant message with usage; sum input + cache*; context limit best-effort
    let mut f = File::open(transcript_path).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    // iterate from end
    for line in s.lines().rev() {
        let t = line.trim(); if t.is_empty() { continue; }
        let parsed: TranscriptLine = match serde_json::from_str(t) { Ok(v) => v, Err(_) => continue };
        if parsed.r#type.as_deref() != Some("assistant") { continue; }
        let usage = parsed.message.and_then(|m| m.usage).unwrap_or(MessageUsage{input_tokens:None,output_tokens:None,cache_creation_input_tokens:None,cache_read_input_tokens:None});
        if let Some(inp) = usage.input_tokens {
            let total_in = inp
                + usage.cache_creation_input_tokens.unwrap_or(0)
                + usage.cache_read_input_tokens.unwrap_or(0);
            // Static context limit lookup
            let context_limit = context_limit_for_model(model_id);
            let pct = ((total_in as f64 / context_limit as f64) * 100.0).round() as u32;
            return Some((total_in, pct.min(100)));
        }
    }
    None
}

fn calc_context_from_entries(entries: &[Entry], session_id: &str, model_id: &str) -> Option<(u64, u32)> {
    let mut filtered: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.session_id.as_deref() == Some(session_id))
        .collect();
    if filtered.is_empty() { return None; }
    filtered.sort_by_key(|e| e.ts);
    let last = filtered.last()?;
    let total_in = last.input + last.cache_create + last.cache_read;
    let limit = context_limit_for_model(model_id);
    let pct = ((total_in as f64 / limit as f64) * 100.0).round() as u32;
    Some((total_in, pct.min(100)))
}

#[derive(Clone)]
struct Entry {
    ts: DateTime<Utc>,
    input: u64,
    output: u64,
    cache_create: u64,
    cache_read: u64,
    cost: f64,
    model: Option<String>,
    session_id: Option<String>,
    msg_id: Option<String>,
    req_id: Option<String>,
}

fn sanitized_project_name(project_dir: &str) -> String {
    project_dir.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

#[derive(Default, Debug, Clone)]
struct GitInfo {
    branch: Option<String>,
    short_commit: Option<String>,
    is_clean: Option<bool>,
    ahead: Option<usize>,
    behind: Option<usize>,
    remote_url: Option<String>,
    is_head_on_remote: Option<bool>,
    worktree_count: Option<usize>,
    is_linked_worktree: Option<bool>,
}

fn read_git_info(start_dir: &Path) -> Option<GitInfo> {
    let repo = gix::discover(start_dir).ok()?;
    let mut info = GitInfo::default();

    // HEAD and short commit id
    let mut head = repo.head().ok()?;
    if let Some(name) = head.referent_name() {
        let short = name.shorten();
        info.branch = Some(short.to_string());
    }
    if let Ok(Some(id)) = head.try_peel_to_id_in_place() {
        let hex = id.to_hex().to_string();
        info.short_commit = Some(hex.chars().take(7).collect());
    }

    // Dirty status via index vs worktree (untracked files do not affect it)
    match repo.is_dirty() {
        Ok(dirty) => info.is_clean = Some(!dirty),
        Err(_) => info.is_clean = None,
    }

    // Remote URL from config
    let cfg = repo.config_snapshot();
    if let Some(url) = cfg.string("remote.origin.url") {
        info.remote_url = Some(url.to_string());
    }

    // Worktree count (primary + linked) and detect if current is a linked worktree
    let mut count = 1usize;
    if let Ok(wts) = repo.worktrees() {
        count += wts.len();
    }
    info.worktree_count = Some(count);
    // Determine if current working dir is a linked worktree by checking if .git is a file
    if let Some(wd) = repo.work_dir() {
        let dotgit = wd.join(".git");
        if dotgit.is_file() {
            info.is_linked_worktree = Some(true);
        } else if dotgit.is_dir() {
            info.is_linked_worktree = Some(false);
        }
    }

    // ahead/behind via revision walks against configured upstream
    if let Some(branch_name) = info.branch.clone() {
        let cfg = repo.config_snapshot();
        let key_remote = format!("branch.{}.remote", branch_name);
        let key_merge = format!("branch.{}.merge", branch_name);
        if let (Some(remote), Some(merge_ref)) = (cfg.string(key_remote.as_str()), cfg.string(key_merge.as_str())) {
            let remote_s = remote.to_string();
            let merge_s = merge_ref.to_string();
            let merge_short = merge_s.strip_prefix("refs/heads/").unwrap_or(merge_s.as_str());
            let upstream_ref = format!("refs/remotes/{}/{}", remote_s, merge_short);
            if let Ok(mut up_ref) = repo.find_reference(upstream_ref.as_str()) {
                if let Ok(up_id) = up_ref.peel_to_id_in_place() {
                    if let Ok(Some(head_id)) = repo.head().ok()?.try_peel_to_id_in_place() {
                        let limit = 50_000usize;
                        let mut head_set = std::collections::HashSet::<String>::new();
                        if let Ok(iter) = head_id.ancestors().all() {
                            for item in iter.flatten() {
                                head_set.insert(item.id.to_string());
                                if head_set.len() >= limit { break; }
                            }
                        }
                        let mut up_set = std::collections::HashSet::<String>::new();
                        if let Ok(iter) = up_id.ancestors().all() {
                            for item in iter.flatten() {
                                up_set.insert(item.id.to_string());
                                if up_set.len() >= limit { break; }
                            }
                        }
                        let ahead = head_set.difference(&up_set).count();
                        let behind = up_set.difference(&head_set).count();
                        info.ahead = Some(ahead);
                        info.behind = Some(behind);
                        info.is_head_on_remote = Some(ahead == 0 && behind == 0);
                    }
                }
            }
        }
    }
    Some(info)
}

fn scan_usage(paths: &[PathBuf], session_id: &str, project_dir: Option<&str>) -> Result<(f64 /*session*/, f64 /*today*/, Vec<Entry>, Option<DateTime<Utc>>, Option<String>)> {
    let today = Local::now().date_naive();
    let mut session_cost = 0.0f64;
    let mut today_cost = 0.0f64;
    let mut entries: Vec<Entry> = Vec::new();
    let mut latest_reset: Option<DateTime<Utc>> = None;
    let mut api_key_source: Option<String> = None;
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for base in paths {
        let root = base.join("projects");
        if !root.is_dir() { continue; }
        // Prefer current project session file if we can derive it
        let mut candidate_files: Vec<PathBuf> = Vec::new();
        if let Some(pd) = project_dir {
            let sanitized = sanitized_project_name(pd);
            let p = root.join(format!("{}.jsonl", sanitized));
            if p.is_file() { candidate_files.push(p); }
        }
        // Fallback: scan all jsonl files
        if candidate_files.is_empty() {
            for entry in globwalk::GlobWalkerBuilder::from_patterns(&root, &["**/*.jsonl"]).build().context("glob")? {
                let entry = match entry { Ok(e) => e, Err(_) => continue };
                candidate_files.push(entry.path().to_path_buf());
            }
        }

        for path in candidate_files {
            let file = match File::open(&path) { Ok(f) => f, Err(_) => continue };
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = match line { Ok(l) => l, Err(_) => continue };
                let t = line.trim(); if t.is_empty() { continue; }
                let v: Value = match serde_json::from_str(t) { Ok(v) => v, Err(_) => continue };
                // detect init system apiKeySource
                if api_key_source.is_none() {
                    if v.get("type").and_then(|s| s.as_str()) == Some("system") && v.get("subtype").and_then(|s| s.as_str()) == Some("init") {
                        if let Some(src) = v.get("apiKeySource").and_then(|s| s.as_str()) {
                            api_key_source = Some(src.to_string());
                        }
                    }
                }
                // detect reset time from API error line or other usage-limit messages with pipe+epoch
                if v.get("isApiErrorMessage").and_then(|b| b.as_bool()) == Some(true) {
                    if let Some(msg) = v.get("message") {
                        if let Some(content) = msg.get("content") {
                            if let Some(arr) = content.as_array() {
                                for c in arr {
                                    if let Some(text) = c.get("text").and_then(|s| s.as_str()) {
                                        if text.contains("Claude AI usage limit reached") {
                                            if let Some(idx) = text.rfind('|') {
                                                if let Ok(epoch) = text[idx+1..].trim().parse::<i64>() {
                                                    if epoch > 0 {
                                                        let dt = DateTime::<Utc>::from_timestamp(epoch, 0).unwrap();
                                                        if latest_reset.map(|x| dt > x).unwrap_or(true) { latest_reset = Some(dt); }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // broader detection: look for assistant/system text containing 'usage limit' and trailing | epoch
                    if let Some(content) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
                        for c in content {
                            if let Some(text) = c.get("text").and_then(|s| s.as_str()) {
                                if text.to_lowercase().contains("usage limit") {
                                    if let Some(idx) = text.rfind('|') {
                                        if let Ok(epoch) = text[idx+1..].trim().parse::<i64>() {
                                            if epoch > 0 {
                                                let dt = DateTime::<Utc>::from_timestamp(epoch, 0).unwrap();
                                                if latest_reset.map(|x| dt > x).unwrap_or(true) { latest_reset = Some(dt); }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // usage line
                let ts = match v.get("timestamp").and_then(|s| s.as_str()) { Some(s)=>s, None=>continue };
                let tsd = match DateTime::parse_from_rfc3339(ts).map(|d| d.with_timezone(&Utc)) { Ok(d)=>d, Err(_)=>continue };
                let msg = match v.get("message") { Some(m)=>m, None=>continue };
                let usage = match msg.get("usage") { Some(u)=>u, None=>continue };
                let input = usage.get("input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
                let output = usage.get("output_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
                let cache_create = usage.get("cache_creation_input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
                let cache_read = usage.get("cache_read_input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
                let mut cost = v.get("costUSD").and_then(|n| n.as_f64()).unwrap_or(0.0);
                // Include web search request charges if present when we compute fallback cost
                let web_search_reqs = usage
                    .get("server_tool_use")
                    .and_then(|o| o.get("web_search_requests"))
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let model = msg.get("model").and_then(|s| s.as_str()).map(|s| s.to_string());
                // Claude Code writes both session_id and sessionId in different places; accept either
                let sid = v
                    .get("sessionId")
                    .or_else(|| v.get("session_id"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                let mid = msg.get("id").and_then(|s| s.as_str()).map(|s| s.to_string());
                let rid = v.get("requestId").or_else(|| v.get("request_id")).and_then(|s| s.as_str()).map(|s| s.to_string());
                let key_left = mid.clone().unwrap_or_else(|| format!("{}|{}|{}|{}|{}", tsd.to_rfc3339(), model.clone().unwrap_or_default(), input, output, cache_read));
                let key_right = rid.clone().unwrap_or_default();
                if !seen.insert((key_left, key_right)) { continue; }
                if cost == 0.0 {
                    if let Some(ref mdl) = model {
                        if let Some(mut p) = pricing_for_model(mdl) {
                            // Large-uncached-input bump for sonnet/haiku-style pricing when total input > 200k
                            let total_in = input + cache_create + cache_read;
                            let no_explicit_env = env::var("CLAUDE_PRICE_INPUT").is_err()
                                || env::var("CLAUDE_PRICE_OUTPUT").is_err()
                                || env::var("CLAUDE_PRICE_CACHE_CREATE").is_err()
                                || env::var("CLAUDE_PRICE_CACHE_READ").is_err();
                            let mdl_l = mdl.to_lowercase();
                            if no_explicit_env && total_in > 200_000 && (mdl_l.contains("sonnet") || mdl_l.contains("haiku")) {
                                // Bump rates to high-volume tier
                                p.in_per_tok = 6e-6;
                                p.out_per_tok = 22.5e-6;
                                p.cache_create_per_tok = 7.5e-6;
                                p.cache_read_per_tok = 0.6e-6;
                            }
                            // Fallback: include cache + web search pricing
                            cost = (input as f64) * p.in_per_tok
                                 + (output as f64) * p.out_per_tok
                                 + (cache_create as f64) * p.cache_create_per_tok
                                 + (cache_read as f64) * p.cache_read_per_tok
                                 + (web_search_reqs as f64) * 0.01; // per-request charge
                        }
                    }
                }
                if sid.as_deref() == Some(session_id) { session_cost += cost; }
                if let Some(d) = parse_iso_date(ts) { if d == today { today_cost += cost; } }
                entries.push(Entry { ts: tsd, input, output, cache_create, cache_read, cost, model, session_id: sid, msg_id: mid, req_id: rid });
            }
        }
    }
    Ok((session_cost, today_cost, entries, latest_reset, api_key_source))
}

#[derive(Default, Clone)]
struct TokenCounts { input: u64, output: u64, cache_create: u64, cache_read: u64 }

#[derive(Clone)]
struct Block {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    actual_end: DateTime<Utc>,
    is_active: bool,
    is_gap: bool,
    entries: Vec<Entry>,
    tokens: TokenCounts,
    cost: f64,
}

fn floor_to_hour_utc(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_minute(0).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap()
}

fn create_block(start: DateTime<Utc>, entries: &[Entry], now: DateTime<Utc>, session_ms: i64, reset: Option<DateTime<Utc>>) -> Block {
    let mut end = start + chrono::TimeDelta::hours(5);
    if let Some(r) = reset { if r < end { end = r; } }
    let actual_end = entries.last().map(|e| e.ts).unwrap_or(start);
    let mut tokens = TokenCounts::default();
    let mut cost = 0.0;
    for e in entries {
        tokens.input += e.input;
        tokens.output += e.output;
        tokens.cache_create += e.cache_create;
        tokens.cache_read += e.cache_read;
        cost += e.cost;
    }
    let is_active = (now - actual_end).num_milliseconds() < session_ms && now < end;
    Block { start, end, actual_end, is_active, is_gap: false, entries: entries.to_vec(), tokens, cost }
}

fn create_gap_block(last_activity: DateTime<Utc>, next_activity: DateTime<Utc>, session_ms: i64) -> Option<Block> {
    // Only for gaps longer than session duration
    if (next_activity - last_activity).num_milliseconds() <= session_ms { return None; }
    let start = last_activity + chrono::TimeDelta::milliseconds(session_ms);
    let end = next_activity;
    Some(Block {
        start,
        end,
        actual_end: start,
        is_active: false,
        is_gap: true,
        entries: Vec::new(),
        tokens: TokenCounts::default(),
        cost: 0.0,
    })
}

fn identify_blocks(mut entries: Vec<Entry>, reset: Option<DateTime<Utc>>) -> Vec<Block> {
    if entries.is_empty() { return vec![]; }
    entries.sort_by_key(|e| e.ts);
    let session_ms = 5 * 60 * 60 * 1_000;
    let now = Utc::now();
    let mut blocks: Vec<Block> = Vec::new();
    let mut current_start: Option<DateTime<Utc>> = None;
    let mut current_entries: Vec<Entry> = Vec::new();
    for e in entries.into_iter() {
        if current_start.is_none() {
            current_start = Some(floor_to_hour_utc(e.ts));
            current_entries.push(e);
            continue;
        }
        let start = current_start.unwrap();
        let last_ts = current_entries.last().unwrap().ts;
        let time_since_start = (e.ts - start).num_milliseconds();
        let since_last = (e.ts - last_ts).num_milliseconds();
        // Split at reset boundary if provided
        if let Some(r) = reset {
            if e.ts >= r && r > start && r < start + chrono::TimeDelta::hours(5) {
                let block = create_block(start, &current_entries, now, session_ms, reset);
                blocks.push(block);
                current_start = Some(r);
                current_entries = vec![e];
                continue;
            }
        }
        if time_since_start > session_ms || since_last > session_ms {
            let block = create_block(start, &current_entries, now, session_ms, reset);
            blocks.push(block);
            // Gap block if needed
            if since_last > session_ms {
                if let Some(gap) = create_gap_block(last_ts, e.ts, session_ms) { blocks.push(gap); }
            }
            // Start next block floored to hour
            current_start = Some(floor_to_hour_utc(e.ts));
            current_entries = vec![e];
        } else {
            current_entries.push(e);
        }
    }
    if let Some(start) = current_start { if !current_entries.is_empty() { blocks.push(create_block(start, &current_entries, now, session_ms, reset)); } }
    blocks
}

fn model_colored_name(model_id: &str, display: &str) -> String {
    let lower = model_id.to_lowercase();
    if lower.contains("opus") { format!("{}", display.bright_magenta()) }
    else if lower.contains("sonnet") { format!("{}", display.bright_yellow()) }
    else if lower.contains("haiku") { format!("{}", display.bright_cyan()) }
    else { format!("{}", display.bright_white()) }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let stdin = read_stdin()?;
    if stdin.is_empty() {
        println!("Claude Code\n{} {}", "❯".cyan(), "[waiting for valid input]".dimmed());
        return Ok(());
    }
    let hook: HookJson = serde_json::from_slice(&stdin).context("parse hook json")?;

    // Prepare header components
    let dir_fmt = format_path(&hook.workspace.current_dir);
    let mdisp = model_colored_name(&hook.model.id, &hook.model.display_name);

    // Compute metrics
    let paths = claude_paths(args.claude_config_dir.as_deref());
    let (session_cost, today_cost, entries, latest_reset, api_key_source) = scan_usage(&paths, &hook.session_id, hook.workspace.project_dir.as_deref()).unwrap_or((0.0, 0.0, Vec::new(), None, None));
    let mut context = calc_context_from_transcript(Path::new(&hook.transcript_path), &hook.model.id);
    let mut context_source: Option<&'static str> = None;
    if context.is_some() { context_source = Some("transcript"); }

    // Build header segments: git (minimal) + model + optional provider hints
    let mut header_parts: Vec<String> = Vec::new();
    // Git info from project_dir or current_dir
    let git_dir = hook.workspace.project_dir.as_deref().unwrap_or(&hook.workspace.current_dir);
    let git_info = read_git_info(Path::new(git_dir));
    if let Some(gi) = git_info.as_ref() {
        let mut git_seg = String::new();
        // worktree indicator
        if gi.is_linked_worktree == Some(true) { git_seg.push_str("wt "); }
        if let (Some(br), Some(sc)) = (gi.branch.as_ref(), gi.short_commit.as_ref()) {
            // branch and short sha
            git_seg.push_str("⎇ ");
            git_seg.push_str(&format!("{}@{}", br, sc));
        } else if let Some(sc) = gi.short_commit.as_ref() {
            git_seg.push_str(&format!("(detached@{})", sc));
        }
        // dirty marker
        if gi.is_clean == Some(false) { git_seg.push('*'); }
        // ahead/behind
        if let (Some(a), Some(b)) = (gi.ahead, gi.behind) {
            if a > 0 { git_seg.push(' '); git_seg.push_str(&format!("↑{}", a)); }
            if b > 0 { if a == 0 { git_seg.push(' '); } git_seg.push_str(&format!("↓{}", b)); }
        }
        if !git_seg.is_empty() {
            header_parts.push(format!("{}{}{}", "[".bright_black(), git_seg.bright_white(), "]".bright_black()));
        }
    }
    // Model segment
    header_parts.push(format!("{}{}{}{}{}{}{}",
        "[".bright_black(),
        "model:".bright_black(),
        mdisp,
        "".to_string(),
        "".to_string(),
        "".to_string(),
        "]".bright_black(),
    ));
    // Optional provider hints grouped (only when --show-provider is set)
    if args.show_provider {
        let mut prov_hint_parts: Vec<String> = Vec::new();
        if let Some(src) = api_key_source.as_ref() {
            prov_hint_parts.push(format!("{}{}", "key:".bright_black(), src.bright_white()));
        }
        // Provider hint from env or deduced from model id
        let prov_disp = if let Ok(provider_env) = env::var("CLAUDE_PROVIDER") {
            match provider_env.to_lowercase().as_str() { "firstparty" => "anthropic".to_string(), other => other.to_string() }
        } else {
            deduce_provider_from_model(&hook.model.id).to_string()
        };
        prov_hint_parts.push(format!("{}{}", "prov:".bright_black(), prov_disp.bright_white()));
        if !prov_hint_parts.is_empty() {
            header_parts.push(format!("{}{}{}", "[".bright_black(), prov_hint_parts.join(" "), "]".bright_black()));
        }
    }
    // Print header line: cwd then segments
    println!("{} {}", dir_fmt.bright_blue(), header_parts.join(" "));

    // Identify session blocks and pick active one (prefer entries matching this session)
    // Plan resolution: CLI args override env; max_tokens overrides tier.
    let (env_tier, env_max_tokens) = plan_from_env();
    let plan_tier_cli: Option<String> = args.plan_tier.map(|t| match t { PlanTierArg::Pro => "pro".to_string(), PlanTierArg::Max5x => "max5x".to_string(), PlanTierArg::Max20x => "max20x".to_string() });
    let plan_max_cli: Option<f64> = args.plan_max_tokens.map(|v| v as f64);
    let plan_tier_final: Option<String> = plan_tier_cli.or(env_tier);
    let plan_max: Option<f64> = plan_max_cli.or(env_max_tokens).or_else(|| {
        if let Some(ref t) = plan_tier_final {
            let base: f64 = 200_000.0;
            let mult = match t.as_str() { "pro" => 1.0, "max5x" => 5.0, "max20x" => 20.0, _ => 0.0 };
            if mult > 0.0 { return Some(base * mult); }
        }
        None
    });
    let blocks = identify_blocks(entries.clone(), latest_reset);
    let mut active: Option<Block> = None;
    // Prefer active block that contains this session_id
    for b in &blocks {
        if b.is_active && !b.is_gap && b.entries.iter().any(|e| e.session_id.as_deref() == Some(&hook.session_id)) { active = Some(b.clone()); break; }
    }
    if active.is_none() {
        for b in &blocks { if b.is_active && !b.is_gap { active = Some(b.clone()); break; } }
    }
    // Fallback: construct the reset-anchored "containing-now" window even if no active block was detected
    let now_utc = Utc::now();
    let (total_cost, total_tokens, noncache_tokens, tpm, tpm_indicator, cost_per_hour, remaining_minutes, usage_percent, projected_percent) = if let Some(ref b) = active {
        let total_tokens = (b.tokens.input + b.tokens.output + b.tokens.cache_create + b.tokens.cache_read) as f64;
        let noncache_tokens = (b.tokens.input + b.tokens.output) as f64;
        let duration_minutes = if b.entries.len() >= 2 { ((b.actual_end - b.entries.first().unwrap().ts).num_seconds().max(0) as f64) / 60.0 } else { 0.0 };
        let (tpm, tpm_ind, cph) = if duration_minutes > 0.0 { (total_tokens / duration_minutes, noncache_tokens / duration_minutes, (b.cost / duration_minutes) * 60.0) } else { (0.0, 0.0, 0.0) };
        let remaining_minutes = ((b.end - now_utc).num_minutes()).max(0) as f64;
        let proj_tokens = total_tokens + tpm * remaining_minutes;
        let usage_percent = plan_max.map(|pm| (total_tokens * 100.0 / pm).max(0.0));
        let projected_percent = plan_max.map(|pm| (proj_tokens * 100.0 / pm).max(0.0));
        (b.cost, total_tokens, noncache_tokens, tpm, tpm_ind, cph, remaining_minutes, usage_percent, projected_percent)
    } else {
        // Construct the window containing now based on latest reset or hour-aligned fallback
        let start = if let Some(r) = latest_reset {
            let five = chrono::TimeDelta::hours(5);
            let delta = now_utc - r;
            let k = (delta.num_seconds() / (5*60*60)).max(0);
            r + chrono::TimeDelta::seconds(k * 5 * 60 * 60)
        } else {
            let local = Local::now();
            let floored = local.with_minute(0).and_then(|d| d.with_second(0)).and_then(|d| d.with_nanosecond(0)).unwrap();
            let h = floored.hour() as i64;
            let back = h % 5;
            (floored - chrono::TimeDelta::hours(back)).with_timezone(&Utc)
        };
        let end = start + chrono::TimeDelta::hours(5);
        let mut window_entries: Vec<&Entry> = entries.iter().filter(|e| e.ts >= start && e.ts < end).collect();
        window_entries.sort_by_key(|e| e.ts);
        let mut toks_in: u64 = 0; let mut toks_out: u64 = 0; let mut toks_cc: u64 = 0; let mut toks_cr: u64 = 0; let mut cost_sum: f64 = 0.0;
        for e in &window_entries { toks_in += e.input; toks_out += e.output; toks_cc += e.cache_create; toks_cr += e.cache_read; cost_sum += e.cost; }
        let total_tokens = (toks_in + toks_out + toks_cc + toks_cr) as f64;
        let noncache_tokens = (toks_in + toks_out) as f64;
        let remaining_minutes = ((end - now_utc).num_minutes()).max(0) as f64;
        // Estimate burn from window first->last if we have >=2 entries
        let duration_minutes = if window_entries.len() >= 2 { ((window_entries.last().unwrap().ts - window_entries.first().unwrap().ts).num_seconds().max(0) as f64) / 60.0 } else { 0.0 };
        let tpm = if duration_minutes > 0.0 { total_tokens / duration_minutes } else { 0.0 };
        let tpm_indicator = if duration_minutes > 0.0 { noncache_tokens / duration_minutes } else { 0.0 };
        let cph = if duration_minutes > 0.0 { (cost_sum / duration_minutes) * 60.0 } else { 0.0 };
        let proj_tokens = total_tokens + tpm * remaining_minutes;
        let usage_percent = plan_max.map(|pm| (total_tokens * 100.0 / pm).max(0.0));
        let projected_percent = plan_max.map(|pm| (proj_tokens * 100.0 / pm).max(0.0));
        (cost_sum, total_tokens, noncache_tokens, tpm, tpm_indicator, cph, remaining_minutes, usage_percent, projected_percent)
    };

    // Fallback context from entries if transcript lacked usage
    if context.is_none() {
        context = calc_context_from_entries(&entries, &hook.session_id, &hook.model.id);
        if context.is_some() { context_source = Some("entries"); }
    }

    if args.json {
        // Machine-readable output for statusline consumption
        // Provider from env or deduced from model id
        let provider_env = env::var("CLAUDE_PROVIDER").ok().map(|s| if s.eq_ignore_ascii_case("firstParty") { "anthropic".to_string() } else { s });
        let provider_final = provider_env.clone().unwrap_or_else(|| deduce_provider_from_model(&hook.model.id).to_string());
        // Plan details from CLI args first, then env
        let (env_tier_json, env_max_tokens_json) = plan_from_env();
        let plan_tier_cli_json: Option<String> = args.plan_tier.map(|t| match t { PlanTierArg::Pro => "pro".to_string(), PlanTierArg::Max5x => "max5x".to_string(), PlanTierArg::Max20x => "max20x".to_string() });
        let plan_tier_json: Option<String> = plan_tier_cli_json.or(env_tier_json);
        let plan_max_json: Option<f64> = args.plan_max_tokens.map(|v| v as f64).or(env_max_tokens_json);
        let reset_iso = latest_reset.map(|d| d.to_rfc3339());
        let (ctx_tokens, ctx_pct) = context.map(|(t,p)| (Some(t as u64), Some(p as u32))).unwrap_or((None, None));
        let ctx_limit = context_limit_for_model(&hook.model.id);
        // Git json fields (present even if nulls to keep schema stable)
        let (git_branch, git_short, git_clean, git_ahead, git_behind, git_on_remote, git_remote_url, git_wt_count, git_is_wt) = if let Some(gi) = git_info {
            (gi.branch, gi.short_commit, gi.is_clean, gi.ahead, gi.behind, gi.is_head_on_remote, gi.remote_url, gi.worktree_count, gi.is_linked_worktree)
        } else { (None, None, None, None, None, None, None, None, None) };
        let block_json = serde_json::json!({
            "cost_usd": (total_cost * 100.0).round() / 100.0,
            "start": active.as_ref().map(|b| b.start.to_rfc3339()),
            "end": active.as_ref().map(|b| b.end.to_rfc3339()),
            "remaining_minutes": (remaining_minutes as i64).max(0),
            "usage_percent": usage_percent.map(|v| (v * 10.0).round()/10.0),
            "projected_percent": projected_percent.map(|v| (v * 10.0).round()/10.0),
            "tokens_per_minute": (tpm * 10.0).round()/10.0,
            "tokens_per_minute_indicator": (tpm_indicator * 10.0).round()/10.0,
            "cost_per_hour": (cost_per_hour * 100.0).round()/100.0,
        });
        let json = serde_json::json!({
            "model": {"id": hook.model.id, "display_name": hook.model.display_name},
            "cwd": hook.workspace.current_dir,
            "project_dir": hook.workspace.project_dir,
            "version": hook.version,
            "provider": {"apiKeySource": api_key_source, "env": provider_final},
            "plan": {"tier": plan_tier_json, "max_tokens": plan_max_json},
            "reset_at": reset_iso,
            "session": {"cost_usd": (session_cost * 100.0).round() / 100.0},
            "today": {"cost_usd": (today_cost * 100.0).round() / 100.0},
            "block": block_json,
            "window": block_json,
            "context": {
                "tokens": ctx_tokens,
                "percent": ctx_pct,
                "limit": ctx_limit,
                "source": context_source
            },
            "git": {
                "branch": git_branch,
                "short_commit": git_short,
                "is_clean": git_clean,
                "ahead": git_ahead,
                "behind": git_behind,
                "is_head_on_remote": git_on_remote,
                "remote_url": git_remote_url,
                "worktree_count": git_wt_count,
                "is_linked_worktree": git_is_wt
            }
        });
        println!("{}", serde_json::to_string(&json)?);
    } else {
        // Line 2
        print!("{} ", "❯".bright_cyan());
        // Labels preference
        let long_labels = matches!(args.labels, LabelsArg::Long);
        // session
        let session_label = if long_labels { "session:" } else { "session:" };
        print!("{}{}{} ", session_label.bright_black(), "$".bold().bright_white(), format_currency(session_cost).bold().bright_white());
        print!("{} ", "·".bright_black().dimmed());
        // today
        let today_label = if long_labels { "today:" } else { "today:" };
        print!("{}{}{} ", today_label.bright_black(), "$".bright_red().bold(), format_currency(today_cost).bright_red().bold());
        print!("{} ", "·".bright_black().dimmed());
        // window (formerly block)
        let window_label = if long_labels { "current window:" } else { "window:" };
        print!("{}{}{} ", window_label.bright_black(), "$".bright_white().bold(), format_currency(total_cost).bright_white().bold());
        print!("{} ", "·".bright_black().dimmed());
        // usage (only if a plan/window max is configured)
        if let (Some(usage_percent), Some(projected_percent)) = (usage_percent, projected_percent) {
            let usage_colored = if usage_percent >= 100.0 { format!("{}%", format!("{:.1}", usage_percent)).red().bold().to_string() }
                else if usage_percent >= 80.0 { format!("{}%", format!("{:.1}", usage_percent)).yellow().bold().to_string() }
                else { format!("{}%", format!("{:.1}", usage_percent)).green().to_string() };
            let proj_colored = if projected_percent >= 100.0 { format!("{}%", format!("{:.1}", projected_percent)).red().bold().to_string() }
                else if projected_percent >= 80.0 { format!("{}%", format!("{:.1}", projected_percent)).yellow().bold().to_string() }
                else { format!("{}%", format!("{:.1}", projected_percent)).green().to_string() };
            print!("{}{}{}{} ", "usage:".bright_black(), usage_colored, "→".bright_black(), proj_colored);
            print!("{} ", "·".bright_black().dimmed());
        }
        // countdown and reset time
        let rem_h = (remaining_minutes as i64) / 60;
        let rem_m = (remaining_minutes as i64) % 60;
        let countdown = if rem_h > 0 { format!("{}h{}m", rem_h, rem_m) } else { format!("{}m", rem_m) };
        let countdown_colored = if remaining_minutes < 30.0 { countdown.red().bold().to_string() } else if remaining_minutes < 90.0 { countdown.yellow().to_string() } else { countdown.white().to_string() };
        print!("{}{} ", "⏳ ".bright_black(), countdown_colored);
        print!("{} ", "·".bright_black().dimmed());
        // Reset clock at window end (active end if available; else computed)
        let window_end_local = if let Some(ref b) = active { b.end.with_timezone(&Local) } else {
            // recompute end like earlier fallback branch
            let now_utc = Utc::now();
            let start = if let Some(r) = latest_reset {
                let delta = now_utc - r;
                let k = (delta.num_seconds() / (5*60*60)).max(0);
                r + chrono::TimeDelta::seconds(k * 5 * 60 * 60)
            } else {
                let local = Local::now();
                let floored = local.with_minute(0).and_then(|d| d.with_second(0)).and_then(|d| d.with_nanosecond(0)).unwrap();
                let h = floored.hour() as i64;
                let back = h % 5;
                (floored - chrono::TimeDelta::hours(back)).with_timezone(&Utc)
            };
            (start + chrono::TimeDelta::hours(5)).with_timezone(&Local)
        };
        let use_12h = match args.time_fmt { TimeFormatArg::H12 => true, TimeFormatArg::H24 => false, TimeFormatArg::Auto => {
            if let Ok(forced) = env::var("CLAUDE_TIME_FORMAT") { forced.trim() == "12" }
            else {
                let lc = env::var("LC_TIME").or_else(|_| env::var("LANG")).unwrap_or_default().to_lowercase();
                lc.contains("en_us")
            }
        }};
        let fmt = if use_12h { "%-I:%M %p" } else { "%H:%M" };
        let reset_disp = window_end_local.format(fmt).to_string();
        let midnight = window_end_local.hour() == 0 && window_end_local.minute() == 0;
        if !use_12h && midnight { // 24h mode hint for next day
            print!("{}{}{} ", "↻ ".bright_black(), reset_disp.bright_white(), " (+1d)".bright_black());
        } else {
            print!("{}{} ", "↻ ".bright_black(), reset_disp.bright_white());
        }
        print!("{} ", "·".bright_black().dimmed());
        // burn
        let burn_val = format!("{}/m", format_tokens(tpm.round() as u64));
        let burn_colored = if tpm_indicator >= 5000.0 { format!("{}", burn_val.red().bold()) } else if tpm_indicator >= 2000.0 { format!("{}", burn_val.yellow()) } else { format!("{}", burn_val.green()) };
        print!("{}{} {} ", "burn:".bright_black(), burn_colored, format!("${}/h", format_currency(cost_per_hour)).yellow());
        print!("{} ", "·".bright_black().dimmed());
        // context
        print!("{}", "context:".bright_black());
        if let Some((tokens, pct)) = context { print!("{}", format!("{} ({}%)", format_tokens(tokens), pct).bright_green()); }
        println!();
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Pricing { in_per_tok: f64, out_per_tok: f64, cache_create_per_tok: f64, cache_read_per_tok: f64 }

fn static_pricing_lookup(model_id: &str) -> Option<Pricing> {
    // Prefer exact/known variants before family heuristics
    let m = model_id.to_lowercase();
    // Opus 4.1
    if m.contains("opus-4-1") {
        let in_pt = 15e-6; // $15 / 1M
        return Some(Pricing { in_per_tok: in_pt, out_per_tok: 75e-6, cache_create_per_tok: in_pt * 1.25, cache_read_per_tok: in_pt * 0.1 });
    }
    // Opus 4 (avoid matching 4.1 above)
    if m.contains("opus-4") {
        let in_pt = 15e-6;
        return Some(Pricing { in_per_tok: in_pt, out_per_tok: 75e-6, cache_create_per_tok: in_pt * 1.25, cache_read_per_tok: in_pt * 0.1 });
    }
    // Sonnet 4 (also catch "claude-4-sonnet")
    if m.contains("sonnet-4") || m.contains("4-sonnet") {
        let in_pt = 3e-6; // $3 / 1M
        return Some(Pricing { in_per_tok: in_pt, out_per_tok: 15e-6, cache_create_per_tok: in_pt * 1.25, cache_read_per_tok: in_pt * 0.1 });
    }
    // Claude 3.7 Sonnet
    if m.contains("3-7-sonnet") {
        let in_pt = 3e-6; // treat like sonnet family
        return Some(Pricing { in_per_tok: in_pt, out_per_tok: 15e-6, cache_create_per_tok: in_pt * 1.25, cache_read_per_tok: in_pt * 0.1 });
    }
    // Claude 3.5 Sonnet
    if m.contains("3-5-sonnet") {
        let in_pt = 3e-6;
        return Some(Pricing { in_per_tok: in_pt, out_per_tok: 15e-6, cache_create_per_tok: in_pt * 1.25, cache_read_per_tok: in_pt * 0.1 });
    }
    // Claude 3.5 Haiku
    if m.contains("3-5-haiku") {
        let in_pt = 0.25e-6;
        return Some(Pricing { in_per_tok: in_pt, out_per_tok: 1.25e-6, cache_create_per_tok: in_pt * 1.25, cache_read_per_tok: in_pt * 0.1 });
    }
    None
}

fn pricing_for_model(model_id: &str) -> Option<Pricing> {
    let m = model_id.to_lowercase();
    // Static per-token prices in USD (per token). Based on Anthropic pricing:
    // - cache write ~ 1.25x input price; cache read ~ 0.1x input price
    // Env overrides take precedence when all four are provided.
    if let (Ok(gi), Ok(go), Ok(gc), Ok(gr)) = (
        env::var("CLAUDE_PRICE_INPUT").map(|s| s.parse::<f64>()),
        env::var("CLAUDE_PRICE_OUTPUT").map(|s| s.parse::<f64>()),
        env::var("CLAUDE_PRICE_CACHE_CREATE").map(|s| s.parse::<f64>()),
        env::var("CLAUDE_PRICE_CACHE_READ").map(|s| s.parse::<f64>()),
    ) {
        if let (Ok(ii), Ok(oo), Ok(cc), Ok(cr)) = (gi, go, gc, gr) {
            return Some(Pricing { in_per_tok: ii, out_per_tok: oo, cache_create_per_tok: cc, cache_read_per_tok: cr });
        }
    }
    // Prefer explicit known model variants
    if let Some(p) = static_pricing_lookup(&m) { return Some(p); }
    // Family heuristics
    if m.contains("opus") {
        let in_pt = 15e-6; // $15 / 1M
        Some(Pricing { in_per_tok: in_pt, out_per_tok: 75e-6, cache_create_per_tok: in_pt * 1.25, cache_read_per_tok: in_pt * 0.1 })
    } else if m.contains("sonnet") {
        let in_pt = 3e-6; // $3 / 1M
        Some(Pricing { in_per_tok: in_pt, out_per_tok: 15e-6, cache_create_per_tok: in_pt * 1.25, cache_read_per_tok: in_pt * 0.1 })
    } else if m.contains("haiku") {
        let in_pt = 0.25e-6; // $0.25 / 1M
        // Follow standard multipliers: cache_create ≈ 1.25x input, cache_read ≈ 0.1x input
        Some(Pricing { in_per_tok: in_pt, out_per_tok: 1.25e-6, cache_create_per_tok: in_pt * 1.25, cache_read_per_tok: in_pt * 0.1 })
    } else {
        None
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000_000 { format!("{:.1}B", n as f64 / 1e9) }
    else if n >= 1_000_000 { format!("{:.1}M", n as f64 / 1e6) }
    else if n >= 1_000 { format!("{:.1}K", n as f64 / 1e3) }
    else { n.to_string() }
}

// Determines the per-block max units (tokens) for usage percent calculations
// Priority:
// 1) CLAUDE_PLAN_MAX_TOKENS (explicit numeric)
// 2) CLAUDE_PLAN_TIER in {pro,max5x,max20x} mapped to 200k * {1,5,20}
// Set none to hide usage percent.
fn plan_from_env() -> (Option<String>, Option<f64>) {
    // Returns (tier_str, max_tokens)
    let tier = env::var("CLAUDE_PLAN_TIER").ok();
    if let Ok(s) = env::var("CLAUDE_PLAN_MAX_TOKENS") {
        if let Ok(v) = s.parse::<f64>() { return (tier, Some(v.max(0.0))); }
    }
    if let Some(ref t) = tier {
        let base: f64 = 200_000.0;
        let mult = match t.to_lowercase().as_str() {
            "pro" => 1.0,
            "max5x" | "max_5x" | "5x" => 5.0,
            "max20x" | "max_20x" | "20x" => 20.0,
            _ => 0.0,
        };
        if mult > 0.0 { return (tier, Some(base * mult)); }
    }
    (tier, None)
}
