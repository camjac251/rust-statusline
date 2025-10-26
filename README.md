# claude_statusline

Lightweight statusline utility for Claude Code sessions. Reads a single JSON line of "hook" input on stdin and emits a one-line, colorized summary (or JSON) with session/window cost, usage, countdown to reset, and Git/project context.

## Features

- Status header with model, optional provider hints, and Git context
- One-line text output with live session/window cost, burn rate, usage, and countdown
- Machine-readable JSON output for bar/statusline integration
- Usage analysis from Claude Code project JSONL history
- **Global usage tracking**: Aggregates `today:` cost across all concurrent Claude Code sessions via SQLite cache
- Persistent caching with mtime-based invalidation for performance
- Optional Git and color support via feature flags for lean builds

## Build

Requires Rust 1.74+ (edition 2021).

- Build:
  - `cargo build --release`

Advanced (optional): feature flags exist in Cargo.toml (default enables both; CI/release use default only):
- `git`: enables git repository inspection using gix
- `colors`: enables colorized output using owo-colors

## Installation

After building, configure Claude Code to use the statusline:

1. Copy the binary and pricing data to your `.claude` directory:

   **Linux/macOS:**
   ```bash
   cp target/release/claude_statusline ~/.claude/
   cp pricing.json ~/.claude/
   ```

   **Windows (PowerShell):**
   ```powershell
   Copy-Item target\release\claude_statusline.exe $env:USERPROFILE\.claude\
   Copy-Item pricing.json $env:USERPROFILE\.claude\
   ```

2. Update your Claude Code `settings.json`:

   **Linux/macOS** (`~/.claude/settings.json`):
   ```json
   {
     "statusLine": {
       "type": "command",
       "command": "/home/<username>/.claude/claude_statusline --window-anchor provider --hints"
     }
   }
   ```

   **Windows** (`C:\Users\<username>\.claude\settings.json`):
   ```json
   {
     "statusLine": {
       "type": "command",
       "command": "C:\\Users\\<username>\\.claude\\claude_statusline.exe --window-anchor provider --hints"
     }
   }
   ```

3. Restart Claude Code to see the statusline in action.

**Note:** You can omit `--window-anchor provider --hints` to use defaults, or customize with other CLI flags (see below).

## Usage

This tool expects a single-line JSON "hook" on stdin (as provided by Claude Code). Example:

```bash
echo '{
  "session_id":"abc123",
  "transcript_path":"/path/to/transcript.jsonl",
  "model":{"id":"claude-3.5-sonnet","display_name":"Claude 3.5 Sonnet"},
  "workspace":{"current_dir":"/path/project","project_dir":"/path/project"},
  "version":"1.0.0"
}' | ./target/release/claude_statusline
```

- Text output (default): compact, colorized summary
- JSON output: add `--json`

```bash
# JSON output for integration
echo '{"session_id":"...","transcript_path":"...","model":{...},"workspace":{...}}' \
  | ./target/release/claude_statusline --json
```

## CLI

Flags and options (also see `--help`):

- `--claude-config-dir <PATHS>` (env: CLAUDE_CONFIG_DIR)
  - Comma-separated list of Claude data roots; defaults to `~/.config/claude` and `~/.claude`
- `--json`
  - Emit JSON instead of colorized text
- `--labels <short|long>`
  - Label verbosity in text output (default: short)
- `--git <minimal|verbose>`
  - Git segment style (reserved for future expansion; current header is compact)
- `--time <auto|12h|24h>`
  - Preferred time display (default: auto)
- `--show-provider`
  - Show provider hints in the header (hidden by default)
- `--hints` (env: `CLAUDE_STATUS_HINTS=1`)
  - Show optional hints: approaching-limit warnings, "resets@" emphasis near end, and auto-compact countdown when context ≥40%

## Environment variables

- `CLAUDE_PROVIDER`:
  - Controls provider display; "firstParty" coerces to "anthropic"
- `CLAUDE_TIME_FORMAT`:
  - `"12"` forces 12h; otherwise auto-detects (e.g., en_US -> 12h)
- `CLAUDE_CONTEXT_LIMIT`:
  - Explicit context window tokens if not recognized from model id
- `CLAUDE_PRICING_PATH`:
  - Path to custom pricing.json file (overrides embedded pricing)
- Pricing overrides (if all are set, they take precedence over all sources):
  - `CLAUDE_PRICE_INPUT`, `CLAUDE_PRICE_OUTPUT`, `CLAUDE_PRICE_CACHE_CREATE`, `CLAUDE_PRICE_CACHE_READ`
- Web search requests are charged at $0.01 per request when `costUSD` is not provided in usage logs

## JSON schema (high level)

Example fields (subject to additions):

```json
{
  "model": {"id": "...", "display_name": "..."},
  "cwd": "/path",
  "project_dir": "/path",
  "version": "1.0.0",
  "provider": {"apiKeySource": "env|keychain|...","env": "anthropic|vertex|bedrock"},
  "reset_at": "2025-08-16T12:00:00Z",
  "session": {"cost_usd": 0.42},
  "today": {"cost_usd": 3.14, "sessions_count": 3},
  "window": {
    "cost_usd": 1.23,
    "start": "2025-08-16T07:00:00Z",
    "end": "2025-08-16T12:00:00Z",
    "remaining_minutes": 85,
    "usage_percent": 12.3, // Only present when OAuth API provides it
    "tokens_per_minute": 1500.0,
    "tokens_per_minute_indicator": 1200.0,
    "cost_per_hour": 1.50
  },
  "context": {"tokens": 12345, "percent": 6, "limit": 200000, "source": "transcript|entries"},
  // Extras for consumers (present when computable):
  //   headroom_tokens: remaining tokens against context limit
  //   eta_minutes: rounded minutes to full context at current non-cache TPM
  "context": {"headroom_tokens": 186655, "eta_minutes": 42}
  "git": {
    "branch": "main",
    "short_commit": "abc1234",
    "is_clean": true,
    "ahead": 0,
    "behind": 0,
    "is_head_on_remote": true,
    "remote_url": "git@github.com:org/repo.git",
    "worktree_count": 1,
    "is_linked_worktree": false
  }
}
```

## Development

- Format: `cargo fmt --all -- --check`
- Lints: `cargo clippy --all-targets --all-features -- -D warnings`
- Tests: `cargo test`

The library surface is exposed via `src/lib.rs` to facilitate integration tests:
- Core modules: cli, display, usage, pricing, utils; git (feature gated)

## Notes

- For lean status bars, consider building without git and/or colors:
  - `cargo build --release --no-default-features`
- If Git header is not desired at runtime, omit `--show-provider` to keep the header minimal.
- **Pricing data**:
  - Embedded at compile-time for zero-configuration operation
  - Release artifacts bundle `pricing.json` for easy updates without recompilation
  - Resolution order: 1) `pricing.json` in cwd, 2) `CLAUDE_PRICING_PATH` env, 3) embedded fallback, 4) env var overrides
- **Global usage tracking and API caching**:
  - `today:` cost aggregates across ALL Claude Code sessions using SQLite cache at `~/.claude/statusline.db`
  - OAuth API responses cached with 60s TTL to reduce redundant API calls across process invocations
  - Mtime-based cache invalidation ensures accurate costs when transcripts update
  - Disable with `CLAUDE_DB_CACHE_DISABLE=1` to revert to per-session tracking
  - Concurrent session support via WAL mode (safe for 10+ simultaneous sessions)
