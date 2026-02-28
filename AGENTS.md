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
```

## Architecture

Pipeline: stdin JSON hook -> transcript parsing -> pricing -> display (text or JSON).

### Modules

| Module | Purpose |
|--------|---------|
| `main.rs` | Entry point |
| `lib.rs` | Library root, public API |
| `cli.rs` | Argument parsing with env var fallbacks |
| `models/hook.rs` | Hook input (`HookMessage`) |
| `models/entry.rs` | Transcript entries |
| `models/block.rs` | Usage blocks |
| `models/message.rs` | Message types |
| `models/git.rs` | Git status structs |
| `models/ratelimit.rs` | Rate limit info |
| `models/beads.rs` | Beads models |
| `models/gastown.rs` | Gas Town models |
| `usage.rs` | Transcript analysis, session/window/daily metrics, burn rates |
| `usage_api.rs` | OAuth usage API client with SQLite-cached responses |
| `pricing.rs` | Model pricing tables (compile-time from `pricing.json`) |
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
- OAuth usage API for utilization percentages and reset times

## Before commits

1. `cargo fmt`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-features`
4. Conventional commits: `type(scope): description`
   - Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`

## CI requirements

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- Tests on Ubuntu, macOS, Windows with stable + beta Rust
- All feature combinations tested
- Binary size < 7MB on Linux
- MSRV: Rust 1.87.0

## Constraints

- **Binary size**: Release < 7MB (CI enforced)
- **MSRV**: 1.87.0, edition 2024
- **Pricing**: Compile-time embedded; override with all four `CLAUDE_PRICE_*` env vars
- **Cache**: In-memory (30s TTL) + SQLite at `~/.claude/statusline.db` (WAL mode, concurrent-safe)
- **Time format**: Auto-detects locale; override with `CLAUDE_TIME_FORMAT` or `--time`
