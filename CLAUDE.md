# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@README.md

## Development Commands

```bash
# Build
cargo build --release                         # Standard release build with all features
cargo build --release --no-default-features   # Lean build without git/colors (statusbar-optimized)

# Test (CI runs all of these)
cargo test --all-features --verbose           # Run all tests including integration tests
cargo test --no-default-features --verbose    # Test minimal build
cargo test --test json_output                 # Run specific integration test file
cargo test usage_blocks                       # Run specific test

# Format & Lint (REQUIRED before commits - CI enforced)
cargo fmt --all -- --check                    # Check formatting (CI MUST pass)
cargo fmt                                     # Auto-format code
cargo clippy --all-targets --all-features -- -D warnings  # Lint with warnings as errors (CI MUST pass)

# Run the utility (expects JSON hook on stdin)
echo '{"session_id":"...","transcript_path":"...","model":{"id":"...","display_name":"..."},"workspace":{"current_dir":"...","project_dir":"..."}}' \
  | ./target/release/claude_statusline

# Run with JSON output
echo '{"session_id":"...","transcript_path":"..."}' | ./target/release/claude_statusline --json

# Run with hints
echo '{"session_id":"...","transcript_path":"..."}' \
  | ./target/release/claude_statusline --hints
```

## Deploying to Local Installations

After making code changes, build and deploy to local Claude Code installations:

```bash
# Build for Linux and deploy
cargo build --release
cp target/release/claude_statusline ~/.claude/claude_statusline.new
mv ~/.claude/claude_statusline.new ~/.claude/claude_statusline

# Build for Windows (WSL) and deploy
cargo build --release --target x86_64-pc-windows-gnu
cp target/x86_64-pc-windows-gnu/release/claude_statusline.exe ~/.claude/
```

**Note:** The Linux binary may be in use by Claude Code's statusline, so copy to a `.new` file first then rename.

## Architecture

This statusline utility processes Claude Code session data through a pipeline of specialized modules:

### Core Data Flow
1. **Hook Input** (`models/hook.rs`) - Receives JSON hook from Claude Code on stdin containing session metadata
2. **Transcript Processing** (`usage.rs`) - Parses JSONL transcript files to extract usage blocks and calculate token consumption
3. **Pricing Calculation** (`pricing.rs`) - Computes costs based on model-specific pricing tiers and token usage (compile-time embedded from `pricing.json`)
4. **Display Generation** (`display.rs`) - Formats output as either colorized text or structured JSON

### Module Responsibilities

- **`cli.rs`** - Command-line argument parsing with environment variable fallbacks (CLAUDE_*)
- **`models/`** - Data structures for hook input, transcript entries, usage blocks, git state, and rate limits
- **`usage.rs`** - Analyzes transcript history to compute session/window/daily metrics and burn rates
- **`pricing.rs`** - Model-specific pricing tables (compile-time embedded from `pricing.json`, overridable via env vars) and cost calculations with cache support
- **`cache.rs`** - In-memory usage caching keyed by session_id + project_dir to avoid re-parsing transcripts
- **`db.rs`** - SQLite-based persistent caching for global usage tracking across multiple concurrent sessions
- **`git.rs`** - Repository inspection using gix (feature-gated) for branch/commit/status context
- **`display.rs`** - Formatting logic for both text (with optional colors) and JSON output modes
- **`utils.rs`** - Time formatting, path resolution, and helper functions

### Feature Flags

The codebase uses Cargo features for optional functionality:
- `git` - Enables repository inspection via gix library (adds ~800KB to binary)
- `colors` - Enables terminal color output via owo-colors (adds ~50KB to binary)
- Both enabled by default; disable with `--no-default-features` for minimal builds (~2.5MB vs ~3.5MB)

### Key Integration Points

- Expects single-line JSON on stdin matching `HookMessage` schema (see `models/hook.rs`)
- Searches for transcript files in Claude config directories (default: `~/.config/claude`, `~/.claude`)
- Supports both 5-hour window tracking (pro/max tiers) and daily usage aggregation
- Provides machine-readable JSON output for statusline/bar integration
- Pricing data: embedded at compile-time from `pricing.json`, overridable via `CLAUDE_PRICE_*` env vars
- Web search requests are charged at $0.01 per request when `costUSD` is not provided in usage logs

## Environment Variables

Configuration via environment variables (also see CLI `--help`):

- **`CLAUDE_CONFIG_DIR`** - Comma-separated list of Claude data roots (default: `~/.config/claude,~/.claude`)
- **`CLAUDE_PROVIDER`** - Controls provider display; "firstParty" coerces to "anthropic"
- **`CLAUDE_TIME_FORMAT`** - `"12"` forces 12h; otherwise auto-detects (e.g., en_US → 12h)
- **`CLAUDE_CONTEXT_LIMIT`** - Explicit context window tokens (fallback when hook doesn't provide `context_window.context_window_size`)
- **`CLAUDE_STATUSLINE_DB_PATH`** - Override default SQLite DB location (default: `~/.claude/statusline.db`)
- **`CLAUDE_DB_CACHE_DISABLE`** - Set to `1` to disable SQLite caching (falls back to scan_usage)
- **`CLAUDE_STATUS_HINTS`** - Set to `1` to show optional hints (approaching-limit warnings, "resets@" emphasis, auto-compact countdown)
- **Pricing overrides** (if all are set, they take precedence over embedded pricing):
  - `CLAUDE_PRICE_INPUT`, `CLAUDE_PRICE_OUTPUT`, `CLAUDE_PRICE_CACHE_CREATE`, `CLAUDE_PRICE_CACHE_READ`

## Testing and Review Expectations

- **CI Requirements** (all must pass):
  - `cargo fmt --all -- --check` - Formatting enforcement
  - `cargo clippy --all-targets --all-features -- -D warnings` - Linting with warnings as errors
  - Tests across Ubuntu, macOS, Windows with stable and beta Rust
  - Tests with all feature combinations: `--all-features`, `--no-default-features`, `--features git`, `--features colors`
  - Binary size check: Release build must be <4MB on Linux
  - MSRV check: Must build with Rust 1.87.0

- **Integration Tests** (`tests/`):
  - `json_output.rs` - JSON schema validation and field presence
  - `usage_blocks.rs` - Usage calculation correctness
  - Use `tempfile` for temporary transcript files

- **Before Commits**:
  1. Run `cargo fmt` to auto-format
  2. Run `cargo clippy --all-targets --all-features -- -D warnings` to check lints
  3. Run `cargo test --all-features` to verify tests pass
  4. Commit with conventional commits: `type(scope): description`
     - Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`

## Integrations

- **Claude Code Hook Integration**:
  - Tool receives JSON on stdin from Claude Code's statusLine command
  - See `models/hook.rs` for full schema with fields: `session_id`, `transcript_path`, `model`, `workspace`, `provider`, `version`, `cost`, `context_window`
  - **Context window support** (Claude Code 2.0.69+): Hook includes `context_window` with `current_usage` and `context_window_size` for accurate context tracking with custom proxy models (Gemini, GPT-5, etc.)

- **Transcript Format**:
  - JSONL files in `~/.config/claude/projects/<project>/transcripts/<session>.jsonl`
  - Each line is a JSON object with `role`, `content`, `usage`, `timestamp`, etc.
  - Usage blocks contain: `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`, `costUSD`

- **JSON Output Schema** (high-level, subject to additions):
  - See README.md for full example
  - Key fields: `model`, `cwd`, `project_dir`, `version`, `provider`, `plan`, `reset_at`, `session`, `today`, `window`, `context`, `git`
  - `window` includes: `cost_usd`, `start`, `end`, `remaining_minutes`, `usage_percent`, `projected_percent`, `tokens_per_minute`, `cost_per_hour`
  - `context` includes: `tokens`, `percent`, `limit`, `source` (`"hook"`, `"transcript"`, or `"entries"`), `headroom_tokens`, `eta_minutes`

## IMPORTANT

- **Binary Size**: Release builds with all features should be <7MB on Linux (CI enforces this)
- **MSRV**: Minimum Supported Rust Version is 1.87.0 (edition 2024)
- **Pricing Data**: Embedded at compile-time from `pricing.json`; override with `CLAUDE_PRICE_*` env vars (all four must be set)
- **Cache Behavior**:
  - In-memory cache: Usage entries cached per `(session_id, project_dir)` with 30s TTL for window calculations
  - SQLite persistent cache at `~/.claude/statusline.db`:
    - Global today cost: Cached with mtime-based invalidation for global usage tracking
    - OAuth API responses: Cached with 60s TTL to reduce API calls across process invocations
  - DB cache enables accurate global usage tracking across multiple concurrent sessions
  - Cache disable: Set `CLAUDE_DB_CACHE_DISABLE=1` to disable SQLite caching (falls back to scan_usage)
- **Git Detection**: Git features are optional; build without `--features git` for lean statusbars
- **Time Format**: Auto-detects locale (en_US → 12h, others → 24h) unless overridden by `CLAUDE_TIME_FORMAT` or `--time`
