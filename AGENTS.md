# AGENTS.md

Instructions for AI coding agents working on this repository.

@README.md

## Commands

```bash
# Build
cargo build --release                         # Release build (all features)
cargo build --release --no-default-features   # Lean build (no git/colors)

# Test
cargo test --all-features --verbose           # All tests
cargo test --no-default-features --verbose    # Minimal build tests
cargo test --test json_output                 # Specific test file
cargo test usage_blocks                       # Specific test

# Lint (REQUIRED before commits -- CI enforced)
cargo fmt --all -- --check                    # Check formatting
cargo fmt                                     # Auto-format
cargo clippy --all-targets --all-features -- -D warnings  # Lint (warnings = errors)

# Run (expects JSON hook on stdin)
echo '{"session_id":"...","transcript_path":"...","model":{"id":"...","display_name":"..."},"workspace":{"current_dir":"...","project_dir":"..."}}' \
  | ./target/release/claude_statusline

# Diagnostics and setup
claude_statusline doctor
claude_statusline doctor --json
claude_statusline init --dry-run
claude_statusline init --refresh-interval 5
```

## Architecture

Pipeline: stdin JSON hook -> transcript parsing -> pricing -> display (text or JSON).

### Modules

| Module | Purpose |
|--------|---------|
| `main.rs` | Entry point |
| `lib.rs` | Library root, public API |
| `cli.rs` | Argument parsing with env var fallbacks |
| `config.rs` | Config file discovery and CLI/env/file precedence |
| `doctor.rs` | Diagnostics and Claude Code `statusLine` installer |
| `models/hook.rs` | Hook input (`HookMessage`) |
| `models/entry.rs` | Transcript entries |
| `models/block.rs` | Usage blocks |
| `models/message.rs` | Message types |
| `models/prompt_cache.rs` | Prompt cache countdown state |
| `models/git.rs` | Git status structs |
| `models/ratelimit.rs` | Rate limit info |
| `models/beads.rs` | Beads models |
| `models/gastown.rs` | Gas Town models |
| `usage.rs` | Transcript analysis, session/window/daily metrics, burn rates |
| `usage_api.rs` | OAuth usage API client with SQLite-cached responses |
| `pricing.rs` | Model pricing tables (compile-time from `pricing.json`) |
| `provenance.rs` | Cost, pricing, and context source metadata |
| `cache.rs` | In-memory usage cache keyed by (session_id, project_dir) |
| `db.rs` | SQLite persistent cache for cross-session usage tracking |
| `window.rs` | Usage window calculations |
| `git.rs` | Repository inspection via gix (feature-gated) |
| `display.rs` | Text (colorized) and JSON output formatting |
| `utils.rs` | Time formatting, path resolution, helpers |
| `beads.rs` | Beads issue tracker integration |
| `gastown.rs` | Gas Town multi-agent orchestration support |

### Feature flags

- `git` -- gix-based repo inspection (~800KB)
- `colors` -- owo-colors terminal output (~50KB)
- Both on by default; `--no-default-features` for ~2.5MB minimal builds

### Key integration points

- Single-line JSON on stdin matching `HookMessage` (see `models/hook.rs`)
- Transcript files in `~/.config/claude` and `~/.claude`
- Pricing embedded from `pricing.json`, overridable via `CLAUDE_PRICE_*` env vars
- Config files are optional: explicit `--config`, project `.claude-statusline.toml`, then `~/.config/claude-statusline/config.toml`; precedence is defaults < config < env < CLI
- `doctor` reports Claude paths, `settings.json`, DB/WAL health, OAuth cache/token availability, config load status, and pricing source without reading hook stdin
- `init` writes/updates the Claude Code `statusLine` command, padding, and `refreshInterval`
- OAuth usage API for utilization percentages and reset times (fallback; hook data is preferred)
- Subagent transcripts in `subagents/agent-*.jsonl` are included in cost calculations
- JSON output includes provenance fields for session cost, today cost, pricing source, context source, and prompt cache countdown state

## Before commits

1. `cargo fmt`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-features`
4. Conventional commits: `type(scope): description`
   - Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`
   - Important: `feat:` and `fix:` commits trigger release PRs via release-plz

## Releasing

Automated via [release-plz](https://release-plz.dev/). **Do not manually bump versions or create tags.**

1. Push to `main` with conventional commits
2. release-plz creates/updates a Release PR (version bump + changelog)
3. Merge the Release PR when ready to ship
4. release-plz creates the git tag + GitHub release; CI builds and uploads binaries

Config: `release-plz.toml` (git-only, no crates.io publish, no CHANGELOG.md)

## CI requirements

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- Tests on Ubuntu, macOS, Windows with stable Rust
- All feature combinations tested
- Binary size < 7MB on Linux
- MSRV: Rust 1.88.0

## Constraints

- **Binary size**: Release < 7MB (CI enforced)
- **MSRV**: 1.88.0, edition 2024
- **Pricing**: Compile-time embedded; override with all four `CLAUDE_PRICE_*` env vars
- **Cache**: In-memory (30s TTL) + SQLite at `~/.claude/statusline.db` (WAL mode, concurrent-safe)
- **Time format**: Auto-detects locale; override with `CLAUDE_TIME_FORMAT` or `--time`
