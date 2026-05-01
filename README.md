<div align="center">

# claude_statusline

**Live cost, usage, burn rate, context, and Git status for Claude Code**

[![CI](https://github.com/camjac251/rust-statusline/actions/workflows/ci.yml/badge.svg)](https://github.com/camjac251/rust-statusline/actions/workflows/ci.yml)
[![Release](https://github.com/camjac251/rust-statusline/actions/workflows/release.yml/badge.svg)](https://github.com/camjac251/rust-statusline/actions/workflows/release.yml)
[![Rust](https://img.shields.io/badge/rust-1.88+-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A fast, single-binary statusline for [Claude Code](https://code.claude.com/docs). Parses session transcripts and the OAuth usage API to show real-time metrics in one line.

[Installation](#installation) · [What It Shows](#what-it-shows) · [CLI](#cli) · [JSON Output](#json-output) · [Architecture](#architecture)

<img src="assets/preview.svg" alt="claude_statusline output preview" width="1000">

</div>

---

## Installation

### Option 1: Homebrew (Recommended)

```bash
brew install camjac251/tap/claude-statusline
```

Upgrades work normally after the initial install:

```bash
brew upgrade claude-statusline
```

Bottles are built for macOS (arm64, x86_64) and Linux (arm64, x86_64). Formulas are updated automatically when new releases are published.

### Option 2: Download Binary

```bash
# Linux x64
curl -fsSL https://github.com/camjac251/rust-statusline/releases/latest/download/claude_statusline-linux-x86_64 \
  -o ~/.local/bin/claude_statusline && chmod +x ~/.local/bin/claude_statusline

# Linux ARM64
curl -fsSL https://github.com/camjac251/rust-statusline/releases/latest/download/claude_statusline-linux-arm64 \
  -o ~/.local/bin/claude_statusline && chmod +x ~/.local/bin/claude_statusline

# macOS Apple Silicon
curl -fsSL https://github.com/camjac251/rust-statusline/releases/latest/download/claude_statusline-macos-arm64 \
  -o ~/.local/bin/claude_statusline && chmod +x ~/.local/bin/claude_statusline

# macOS Intel
curl -fsSL https://github.com/camjac251/rust-statusline/releases/latest/download/claude_statusline-macos-x86_64 \
  -o ~/.local/bin/claude_statusline && chmod +x ~/.local/bin/claude_statusline
```

### Option 3: Build from Source

Requires Rust 1.88+:

```bash
git clone https://github.com/camjac251/rust-statusline
cd rust-statusline
cargo build --release
cp target/release/claude_statusline ~/.local/bin/
```

### Configure Claude Code

Add to `~/.claude/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "claude_statusline",
    "padding": 0,
    "refreshInterval": 5
  }
}
```

`padding` and `refreshInterval` are Claude Code settings. `claude_statusline` just renders the current snapshot when Claude Code invokes it.

Claude Code truncates long footer output, so `claude_statusline` now prefers a more compact, Claude-safe layout unless there is clear room for the richer two-line view.

Restart Claude Code. Done.

---

## What It Shows

| Metric | Description |
|--------|-------------|
| **session** | Cost of the current session (includes subagent costs) |
| **today** | Aggregated cost across all concurrent sessions (via SQLite) |
| **window** | Cost within the current 5-hour usage window |
| **usage%** | OAuth-reported utilization and projected usage |
| **burn** | Tokens per minute and cost per hour |
| **context** | Token count and percentage of context window used |
| **reset** | Time remaining until usage window reset |
| **git** | Branch, commit, dirty state, ahead/behind |
| **workspace** | Added workspace dirs and linked worktree hints from Claude Code |

---

## How It Works

```mermaid
flowchart LR
    CC[Claude Code] -->|stdin JSON| SL[claude_statusline]

    subgraph Pipeline
        direction TB
        SL --> TP[Transcript Parser]
        SL --> OA[OAuth API Client]
        TP -->|JSONL files| METRICS[Token counts\nCosts\nBurn rates]
        OA -->|usage endpoint| UTIL[Utilization %\nReset times]
    end

    subgraph Cache
        direction TB
        METRICS --> MEM[In-Memory\n30s TTL]
        METRICS --> DB[(SQLite\nWAL mode)]
        UTIL --> DB
    end

    DB --> OUT[Display]
    MEM --> OUT
    OUT -->|colorized text| STDOUT[stdout]
    OUT -->|--json| JSON[structured JSON]
```

Pricing is embedded at compile time from `pricing.json`. The OAuth API is optional -- if no credentials are available, the tool falls back to transcript-only metrics.

---

## CLI

```
claude_statusline [OPTIONS]
claude_statusline doctor [OPTIONS]
claude_statusline init [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--json` | Emit structured JSON instead of colorized text |
| `--config <PATH>` | Load a config file |
| `--no-config` | Disable config file loading |
| `--no-hints` | Disable status hints (on by default) |
| `--no-prompt-cache` | Disable prompt-cache countdown |
| `--prompt-cache-ttl-seconds <N>` | Fallback TTL when transcripts only expose aggregate cache creation (default: 300) |
| `--labels <short\|long>` | Label verbosity (default: short) |
| `--time <auto\|12h\|24h>` | Time format (default: auto-detect from locale) |
| `--window-anchor <provider\|log>` | Window alignment (default: provider) |
| `--window-scope <global\|project>` | Window cost scope (default: global) |
| `--burn-scope <session\|global>` | Burn rate scope (default: session) |
| `--show-provider` | Show provider/key source in header |
| `--show-provenance` | Show cost/pricing source details in text output |
| `--show-breakdown` | Show per-token-type breakdown and web search count |
| `--no-gastown` | Disable Gas Town multi-agent display |
| `--claude-config-dir <PATHS>` | Override Claude data roots (comma-separated) |

### Setup and diagnostics

```bash
claude_statusline doctor
claude_statusline doctor --json
claude_statusline init
claude_statusline init --dry-run
claude_statusline init --refresh-interval 5
```

`doctor` checks Claude config paths, `settings.json`, SQLite cache health, OAuth cache/token availability, config loading, and pricing lookup provenance without reading statusline stdin.

`init` writes the Claude Code `statusLine` block to `settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "claude_statusline",
    "padding": 0,
    "refreshInterval": 5
  }
}
```

### Config file

Config files are optional. Precedence is:

```text
defaults < config file < environment < CLI
```

Discovery order:

1. `--config <PATH>` or `CLAUDE_STATUSLINE_CONFIG`
2. `./.claude-statusline.toml`
3. `~/.config/claude-statusline/config.toml`

Supported keys mirror the stable CLI options:

```toml
[display]
labels = "long"
git = "verbose"
show_provenance = true
show_breakdown = true
prompt_cache = true
prompt_cache_ttl_seconds = 300
truecolor = true
window_scope = "global"
burn_scope = "session"
window_anchor = "provider"
```

### Environment Variables

| Variable | Effect |
|----------|--------|
| `CLAUDE_STATUSLINE_CONFIG=...` | Explicit config file path |
| `CLAUDE_STATUS_HINTS=0` | Disable status hints (on by default) |
| `CLAUDE_PROMPT_CACHE=0` | Disable prompt-cache countdown |
| `CLAUDE_PROMPT_CACHE_TTL_SECONDS=N` | Override prompt-cache TTL |
| `CLAUDE_TIME_FORMAT=12` | Force 12-hour time |
| `CLAUDE_CONTEXT_LIMIT=N` | Override context window size (tokens) |
| `CLAUDE_PROVIDER=...` | Override provider display (`firstParty` becomes `anthropic`) |
| `CLAUDE_CONFIG_DIR=...` | Comma-separated list of Claude data roots |
| `CLAUDE_DB_CACHE_DISABLE=1` | Disable SQLite cache, fall back to per-session scanning |
| `CLAUDE_STATUSLINE_FETCH_USAGE=0` | Disable OAuth usage API calls |
| `CLAUDE_PRICE_INPUT` | Override input token price (all four must be set) |
| `CLAUDE_PRICE_OUTPUT` | Override output token price |
| `CLAUDE_PRICE_CACHE_CREATE` | Override cache creation token price |
| `CLAUDE_PRICE_CACHE_READ` | Override cache read token price |

---

## JSON Output

Pass `--json` for machine-readable output. Key fields:

```json
{
  "model": { "id": "claude-opus-4-6", "display_name": "Claude Opus 4.6" },
  "workspace": {
    "current_dir": "/repo",
    "project_dir": "/repo",
    "added_dirs": ["/repo/docs"],
    "git_worktree": "feature/footer"
  },
  "session": {
    "cost_usd": 0.42,
    "cost_source": "transcript_result",
    "subagents": [
      { "agent_id": "a1234567890abcdef", "cost_usd": 0.15, "input_tokens": 50000, "output_tokens": 2000 }
    ]
  },
  "today": { "cost_usd": 3.14, "cost_source": "db_global_usage" },
  "window": {
    "cost_usd": 1.23,
    "remaining_minutes": 161,
    "usage_percent": 12.3,
    "tokens_per_minute": 1500.0,
    "cost_per_hour": 1.50
  },
  "context": {
    "tokens": 12345,
    "percent": 6,
    "limit": 200000,
    "usable_limit": 168000,
    "usable_percent": 8,
    "headroom_tokens": 187655,
    "eta_minutes": 42
  },
  "prompt_cache": {
    "ttl_seconds": 300,
    "age_seconds": 60,
    "write_age_seconds": 180,
    "read_age_seconds": 60,
    "remaining_seconds": 120,
    "percent_remaining": 40.0,
    "cache_read_input_tokens": 8000,
    "last_activity_at": "2026-05-01T12:02:00+00:00",
    "last_cache_write_at": "2026-05-01T12:00:00+00:00",
    "last_cache_read_at": "2026-05-01T12:02:00+00:00",
    "buckets": [
      { "kind": "5m", "input_tokens": 5000, "ttl_seconds": 300, "remaining_seconds": 120 }
    ]
  },
  "provenance": {
    "session_cost": "transcript_result",
    "today_cost": "db_global_usage",
    "pricing": "embedded",
    "context": "hook"
  },
  "git": {
    "branch": "main",
    "short_commit": "a3f1c2b",
    "is_clean": true,
    "ahead": 0,
    "behind": 0
  },
  "remote": {
    "session_id": "remote-abc"
  }
}
```

Full schema includes `provider`, `plan`, `reset_at`, `session.subagents`, `prompt_cache`, `provenance`, `git.remote_url`, `git.worktree_count`, `git.is_linked_worktree`, nested `workspace.*`, optional `remote.session_id`, and token breakdowns per window. Top-level `cwd` and `project_dir` remain as compatibility aliases. Fields are added over time; consumers should tolerate unknown keys.

---

## Architecture

```
src/
├── main.rs          # Entry point
├── lib.rs           # Library root, public API
├── cli.rs           # Argument parsing with env var fallbacks
├── config.rs        # Config file discovery and precedence
├── doctor.rs        # Diagnostics and statusLine installer
├── models/          # Data structures
│   ├── hook.rs      # Hook input (HookMessage)
│   ├── entry.rs     # Transcript entries
│   ├── block.rs     # Usage blocks
│   ├── message.rs   # Message types
│   ├── git.rs       # Git status
│   ├── ratelimit.rs # Rate limit info
│   ├── beads.rs     # Beads models
│   └── gastown.rs   # Gas Town models
├── usage.rs         # Transcript analysis, session/window/daily metrics, burn rates
├── usage_api.rs     # OAuth usage API client with SQLite-cached responses
├── pricing.rs       # Model pricing tables (compile-time from pricing.json)
├── provenance.rs    # Cost/pricing/context source metadata
├── cache.rs         # In-memory usage cache (30s TTL)
├── db.rs            # SQLite persistent cache (WAL mode, concurrent-safe)
├── display.rs       # Text (colorized) and JSON output formatting
├── window.rs        # Usage window calculations
├── git.rs           # Repository inspection via gix (feature-gated)
├── utils.rs         # Time formatting, path resolution, helpers
├── beads.rs         # Beads issue tracker integration
└── gastown.rs       # Gas Town multi-agent orchestration support
```

### Feature Flags

| Feature | Default | Effect | Size |
|---------|---------|--------|------|
| `git` | on | Git branch/commit/status via [gix](https://github.com/GitoxideLabs/gitoxide) | ~800 KB |
| `colors` | on | Terminal colors via [owo-colors](https://github.com/jam1garner/owo-colors) | ~50 KB |

Build without both for a minimal ~2.5 MB binary:

```bash
cargo build --release --no-default-features
```

---

## Development

```bash
cargo fmt                                              # format
cargo clippy --all-targets --all-features -- -D warnings  # lint
cargo test --all-features --verbose                    # test
```

CI runs all tests across Ubuntu, macOS, and Windows with stable and beta Rust, all feature combinations, and enforces a 7 MB binary size limit.

---

## License

[MIT](LICENSE)
