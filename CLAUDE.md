# rust-statusline

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
| `models/subagent.rs` | Subagent sidecar metadata and enriched per-agent usage rows |
| `models/workflow.rs` | Workflow run-state and remote-agent task models |
| `usage.rs` | Transcript analysis, session/window/daily metrics, burn rates |
| `usage_api.rs` | OAuth usage API client with SQLite-cached responses; honors proxy env and `NODE_EXTRA_CA_CERTS`, reports egress route, backs off on the endpoint's own `retry-after` |
| `pricing.rs` | Model pricing tables (compile-time from `pricing.json`) |
| `provenance.rs` | Cost, pricing, and context source metadata |
| `db.rs` | SQLite persistent cache and usage event ledger for cross-session usage tracking |
| `window.rs` | Usage window calculations |
| `git.rs` | Repository inspection via gix (feature-gated) |
| `display.rs` | Text (colorized) and JSON output formatting |
| `utils.rs` | Time formatting, path resolution, helpers |
| `beads.rs` | Beads issue tracker integration |
| `gastown.rs` | Gas Town multi-agent orchestration support |
| `workflow.rs` | Workflow-progress and remote-agent task discovery (session-scoped) |
| `subagent_statusline.rs` | Live agent-panel task payload parsing and JSONL row rendering |
| `tokens.rs` | Color tokens, the shared effort tier scale, OSC 8 hyperlinks, and display-column width |

### Feature flags

- `git` -- gix-based repo inspection (~800KB)
- `colors` -- owo-colors terminal output (~50KB)
- Both on by default; `--no-default-features` for ~2.5MB minimal builds

### Key integration points

- Single-line JSON on stdin matching `HookMessage` (see `models/hook.rs`), or a `subagentStatusLine` task payload auto-detected by its `tasks` array; agent-panel mode answers with per-task `{id, content}` JSONL (or a structured `{tasks: [...]}` object with `--json`); rows derive a tokens-per-minute burn chip from `tokenSamples` (one sample per five-second tick) on the richest width variant, and `--json` rows carry `tokens_per_minute` when derivable
- Claude Code drops **every** decoration for the tick when the agent-panel command exits non-zero, so that path never fails the whole batch on one bad row: tasks are parsed individually and an unreadable one is skipped (keeping Claude Code's own rendering) while its siblings still decorate, `columns` falls back when absent, and `SubagentEffort::Other` swallows any effort shape the payload grows
- Both surfaces resolve truecolor through `display::is_truecolor_enabled` and paint effort through `tokens::effort_chip`. One binary in one terminal must not render the footer and the agent panel differently, which is what happened when agent-panel mode read the raw `--truecolor` flag and ignored `COLORTERM`/`CLAUDE_TRUECOLOR`
- `tokens::effort_chip` owns the tier scale. It accepts the aliases Claude Code accepts (`med`, `ultracode`), and renders an unrecognized named tier muted rather than dropping it, so a patched cli.js that grows a level still shows one. Only an empty label, an integer level, or a non-label shape yields no chip. As of cli.js 2.1.223 the emitted set is exactly `low|medium|high|xhigh|max` on both stock and cc-enhanced builds; `none` is unreachable but kept as a tier
- Path segments carry OSC 8 hyperlinks (`tokens::hyperlink`). Claude Code parses them out of statusline output and re-emits them on capable terminals, degrading to plain text elsewhere, so there is no terminal detection here -- only the `--no-hyperlinks` opt-out. `tokens::strip_ansi` must understand OSC as well as CSI or the URL is charged to the width budget
- Transcript files in `~/.config/claude` and `~/.claude`
- Pricing embedded from `pricing.json`, overridable via `CLAUDE_PRICE_*` env vars
- Config files are optional: explicit `--config`, project `.claude-statusline.toml`, then `~/.config/claude-statusline/config.toml`; precedence is defaults < config < env < CLI
- `doctor` reports Claude paths, `settings.json` (`statusLine` plus the optional `subagentStatusLine`, whose absence never degrades `ok`), DB/WAL health, OAuth cache/token availability, the effective usage-cache TTL, any unmapped usage-response fields, the usage API egress route (direct or proxy, from `HTTPS_PROXY`/`NO_PROXY`, plus `NODE_EXTRA_CA_CERTS` trust), config load status, and pricing source without reading hook stdin
- Usage API HTTP honors `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY` (via ureq env defaults, inherited from Claude Code's environment) and `NODE_EXTRA_CA_CERTS` (system roots + extra bundle via `rustls-native-certs`); `resolve_usage_egress` powers the route shown by `doctor`/`--debug`
- `init` writes/updates the Claude Code `statusLine` command, padding, and `refreshInterval`, plus the `subagentStatusLine` block (`type` and `command` only), preserving extra keys on existing objects and refusing non-object values without `--force`
- OAuth usage API for utilization percentages and reset times (fallback; hook data is preferred)
- `subscription_usage_applies()` gates the usage cluster, **not** `is_direct_claude_api(model_id)`. The five-hour, weekly, scoped, and extra-usage rows describe the account, so keying them on the model of the turn in flight blanked the whole cluster the moment a mixed-model launcher routed one reply through `clodex:openai-oauth:*`, and restored it on the next Claude reply. Only a non-Anthropic `ANTHROPIC_BASE_URL` means the numbers genuinely do not apply. Remember that `main.rs` overwrites `hook.model.id` with the transcript's last billed model, so the gate follows the last *reply*, not the picker
- `merge_hook_usage_with_api` must not return the hook summary wholesale when the API response is stale. The hook carries only `five_hour`/`seven_day`; `limits[]`, `spend`, and `extra_usage` have no hook counterpart, so returning early deleted the only copy and blinked `fable:`/`ex:` off the line for every fetch-lock window and every 120s negative-cache backoff while `usage:`/`7d:` stayed put. Carry those rows plus `fetched_at` (so `up:` reports their age) and take `stale` from the hook, since that flag drives the `~usage:` marker on the five-hour figure the merge is returning
- Carrying stale rows forward is only safe while their windows are still running. `UsageSummary::clear_elapsed_windows` runs on every `get_usage_summary` return path (`without_elapsed_windows`) and blanks the **OAuth-only** rows whose `resets_at` has passed: `limits[]`, `seven_day_opus`/`seven_day_sonnet`/`seven_day_oauth_apps`/`seven_day_cowork`, and the rolling codename slots. Before this, `any_window_reset_elapsed` was a refetch *trigger* only, and the refetch cannot always land: a sibling session holds the 10s fetch lock, a 429 arms the backoff for up to `MAX_RETRY_AFTER_SECONDS`, or the token expires and `GET_STALE_API_CACHE` (no age bound) serves the same row indefinitely. The result was `fable:` pinned at its pre-reset number while the hook-fed `usage:`/`7d:` beside it kept moving, since only the hook rows were checked for an elapsed reset. `limits[]` rows keep their `resets_at` and lose only `percent`, so `--json` still reports which window ended; `UsageLimit` rows clear `resets_at` too, or a past reset freezes the countdown at `0m` instead of rolling forward from the window anchor. A row without a `resets_at` cannot be judged and is left alone
- `window` and `seven_day` are **deliberately excluded** from that pass. `main.rs` already owns their staleness and makes the opposite call: keep the last number and set `stale` so the label renders `~usage:`. That restore is gated on hook `rate_limits`, which the API-primary path does not have, so clearing them here deleted the `usage:` and `7d:` tokens outright for a session whose hook sends no rate limits. Covered by `stale_api_without_hook_keeps_a_marked_five_hour_figure_past_its_reset`
- The cache bypass keys on `any_window_reset_since_fetch`, not `any_window_reset_elapsed`: only a reset that landed *after* `fetched_at` justifies skipping the positive cache. A reset the fetch itself already saw is the endpoint's latest word, and bypassing on it refetches every invocation (5s refresh, 10s fetch lock) against a ~5-per-300s account budget, ending in the 429 that pins the very numbers the bypass was meant to replace. It is a strict subset of `any_window_reset_elapsed` over the same `rolling_resets()`, so it can only lower request volume, and the display guard uses the wider predicate — anything the narrowed trigger misses is blanked rather than shown pinned. `fetched_at` is stamped with the instant the request went out, not its arrival, so a reset landing mid-flight still reads as new
- The usage endpoint budgets roughly 5 requests per fixed 300s window **per account**. `CACHE_TTL_SECONDS` (300, overridable via `CLAUDE_USAGE_CACHE_TTL_SECONDS`, floored at 60) keeps one machine to one request per window; a machine-local SQLite fetch lock keeps concurrent sessions to one call; a 429 arms the negative cache from the response's `retry-after`. The agent is built with `http_status_as_error(false)` specifically so a 429 arrives as a readable response instead of an opaque transport error
- `UsageSummary::fetched_at` is stamped on the fetch path, so it round-trips through the cached JSON and describes the fetch rather than the read. The `up:` display token derives from it and stays hidden until `is_past_refresh_window`, making it a fault signal rather than a permanent fixture. A cache row written before this field existed reports no age instead of a fabricated one
- Unrecognized usage-response keys land in `UsageSummary::unknown_fields` via a `serde(flatten)` catch-all rather than being dropped. Codename limit slots (`seven_day_omelette`, `tangelo`, `iguana_necktie`, `omelette_promotional`, `nimbus_quill`, `amber_ladder`) are captured into `codename_limits` when non-null. Before wiring a new slot into `ROLLING_CODENAME_LIMIT_KEYS`, decide whether it is a rolling window or a one-time expiry: only rolling ones belong there, or the slot's value is blanked the moment the credit lapses (and, for a legacy cache row with no `fetched_at`, the cache invalidates on every invocation once the date passes)
- Subagent transcripts in `subagents/agent-*.jsonl` are included in cost calculations
- `session.subagents` rows are enriched from `agent-<id>.meta.json`. Its fields are **conditional, not guaranteed**: measured over 5411 local sidecars, `description` 1950, `spawnDepth` 942, `toolUseId` 526, `model` 235, `name` 133, `parentAgentId` 21. Do not conclude "enrichment is broken" from a small sample. `agent-<id>.forked-skill.json` is a **second sidecar** (`{skillName, attributionName, effort?, frozenCommandDenies?}`) and the only artifact identifying a forked skill, since the fork's `meta.json` reports the type it runs as. `spawnedByWorkflowRunId` matched the containing `workflows/wf_*/` directory in 255/255 cases, so directory-derivation stays the source for `workflow_run_id`
- The usable width is `COLUMNS - 12` (`TERMINAL_MARGIN` 4 + `CLAUDE_FOOTER_RESERVE` 8), **measured, not guessed**. `CLAUDE_STATUSLINE_RULER` renders a self-labelling column ruler into a live TUI; at 80/120/200/320 columns Claude Code granted `COLUMNS - 3` every time, matching the footer container's `paddingLeft: 2` + `paddingRight: 1` (2 while a notification row is up). The right-hand mode-label cluster is a `flexWrap: "wrap"` sibling, not a competitor: at 200 columns in plan mode the ruler still took 197 and "plan mode on" rendered on the row below. The remaining 8 is slack for a terminal narrowed between refreshes, since Claude Code does not re-run the command on resize. Re-measure with the ruler before changing either constant
- Width is measured in **display columns** via `tokens::visible_width`/`truncate_to_width`, not `char`s, because Claude Code lays both surfaces out with a real width function. Counting chars under-measures CJK and over-measures emoji sequences, and the inputs (agent names, model-written panel task labels) are outside our control. Every truncation runs on unstyled text before painting, so a cut can never land inside an escape sequence
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
- MSRV: Rust 1.95.0

## Constraints

- **MSRV**: 1.95.0, edition 2024
- **Pricing**: Compile-time embedded; override with all four `CLAUDE_PRICE_*` env vars
- **Cache**: SQLite at `~/.claude/statusline.db` with session rows and a usage event ledger (WAL mode, concurrent-safe)
- **Time format**: Auto-detects locale; override with `CLAUDE_TIME_FORMAT` or `--time`
