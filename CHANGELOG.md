# Changelog

## Unreleased

## [1.4.1](https://github.com/camjac251/rust-statusline/compare/v1.4.0...v1.4.1) - 2026-05-01

### Fixed

- *(prompt-cache)* detect ttl buckets from transcripts

## [1.4.0](https://github.com/camjac251/rust-statusline/compare/v1.3.3...v1.4.0) - 2026-05-01

### Added

- *(cli)* add diagnostics config and setup

## [1.3.3](https://github.com/camjac251/rust-statusline/compare/v1.3.2...v1.3.3) - 2026-05-01

### Fixed

- *(usage)* align cost calculation with Claude Code

## [1.3.2](https://github.com/camjac251/rust-statusline/compare/v1.3.1...v1.3.2) - 2026-04-28

### Fixed

- *(deps)* update rust crate gix to 0.82.0

## [1.3.1](https://github.com/camjac251/rust-statusline/compare/v1.3.0...v1.3.1) - 2026-04-20

### Fixed

- *(display)* hide redundant workspace tags

## [1.3.0](https://github.com/camjac251/rust-statusline/compare/v1.2.1...v1.3.0) - 2026-04-18

### Added

- *(display)* fit statusline to claude footer

### Fixed

- *(usage)* encode sha2 suffix portably

### Other

- *(deps)* update Rust packages

## [1.2.1](https://github.com/camjac251/rust-statusline/compare/v1.2.0...v1.2.1) - 2026-04-01

### Fixed

- *(ci)* remove redundant workflow_dispatch that races with push

## [1.2.0](https://github.com/camjac251/rust-statusline/compare/v1.1.1...v1.2.0) - 2026-03-27

### Added

- *(usage)* include subagent costs in all metrics

### Other

- add Windows arm64 build target
- auto-merge release-plz PRs and trigger release
- replace mise with Homebrew as recommended install method

## [1.1.1](https://github.com/camjac251/rust-statusline/compare/v1.1.0...v1.1.1) - 2026-03-21

### Fixed

- *(display)* round percentages to avoid fp noise like 7.0%

## [1.1.0](https://github.com/camjac251/rust-statusline/compare/v1.0.14...v1.1.0) - 2026-03-21

### Added

- *(pricing)* update pricing data, add fast mode support, session-focused data sourcing

### Fixed

- *(pricing)* replace unwrap with let-else to satisfy clippy

## [1.0.14](https://github.com/camjac251/rust-statusline/compare/v1.0.13...v1.0.14) - 2026-03-20

### Other

- *(display)* compact statusline - drop warning symbols, inline reset

## [1.0.13](https://github.com/camjac251/rust-statusline/compare/v1.0.12...v1.0.13) - 2026-03-20

### Fixed

- *(ci)* use actions/checkout for homebrew-tap push auth

### Other

- Merge pull request #10 from camjac251/renovate/actions-attest-build-provenance-4.x

## [1.0.12](https://github.com/camjac251/rust-statusline/compare/v1.0.11...v1.0.12) - 2026-03-20

### Fixed

- *(release)* remove publish=false from Cargo.toml

## [1.0.11](https://github.com/camjac251/rust-statusline/compare/v1.0.10...v1.0.11) - 2026-03-20

### Fixed

- *(release)* add explicit publish=false to release-plz config
- *(release)* use publish_no_verify instead of invalid cargo_package field
- *(release)* disable cargo package verify in release-plz
- *(ci)* add publish = false to prevent crates.io publish attempts

### Other

- *(release)* add changelog, attestation, and homebrew tap automation
- add workflow_dispatch trigger for manual binary builds

### Bug Fixes

- Add publish = false to prevent crates.io publish attempts
## [1.0.10] - 2026-03-15

### Bug Fixes

- Update rust crate gix to 0.80
- Update rust crate rusqlite to 0.39
- Correct context window detection for [1m] models and align percentage with CLI
- Disable semver checks for binary-only crate
## [1.0.9] - 2026-03-09

### Bug Fixes

- Prefer transcript model over hook model to prevent cross-session bleed
## [1.0.8] - 2026-03-08

### Bug Fixes

- Increase usage API cache TTL to reduce 429 rate limits
## [1.0.7] - 2026-03-08

### Bug Fixes

- Add serde(default) to UsageSummary for cache compatibility
## [1.0.6] - 2026-03-08

### Features

- Add effort level display and hide default output style

### Refactoring

- Centralize colors into token system
## [1.0.4] - 2026-03-08

### Bug Fixes

- Convert extra_usage cents to dollars, add seven_day_cowork field
## [1.0.3] - 2026-03-08

### Bug Fixes

- Add fetch lock to prevent concurrent API calls at cache boundary
## [1.0.2] - 2026-03-07

### Bug Fixes

- Prevent OAuth usage API rate-limit death spiral
## [1.0.0] - 2026-02-28

### Bug Fixes

- Correct Haiku 3.5 rates, prefer SDK session cost, simplify tiered pricing
- Smooth usage projections
- Remove hardcoded path and embed pricing.json for cross-platform support
- Normalize all reset times to nearest hour
- Prevent stale usage after midnight rollover
- Prevent stale session data on new session start
- Add Opus 4.5 with correct $5/$25 pricing
- Show time for 7d reset when under 24 hours
- Use hook context_window_size in text output for proxy models
- Correct mail query and add --no-gastown flag
- Add missing Command import for macOS keychain

### Features

- Add --hints flag, --plan-profile, and settings.json overrides
- Add session cost-per-hour, lines delta, and remaining usage percent
- Add pricing.json config, in-memory usage cache, and file mtime optimization
- Auto-detect plan tier from usage and improve block boundary detection
- Add session-scoped token breakdown and active block to JSON output
- Add window anchor mode, rate limit display, and offline-only operation
- Parse context warnings, add complexity weights, and use fixed reset hours
- Add system overhead tracking and usable context limit calculation
- Surface oauth usage limits
- Integrate cc-sessions detection and display
- Account for output token reserves in context limits
- Show context limit in usage display
- Show output reserve usage when over usable limit
- Move lines delta to git header, fix hints logic
- Show compact trigger ETA and brighten model colors
- Warn when approaching output token reserve
- Add CLAUDE_AUTOCOMPACT_PCT_OVERRIDE and MAX_OUTPUT_CAPABILITY support
- Add tiered pricing system and normalize reset times
- Implement SQLite-based global usage and API caching
- Cache claude user agent and use sqlite metadata
- Add macOS Keychain support for OAuth credentials
- Add responsive terminal width detection
- Overhaul color scheme, simplify layout, add truecolor detection
- Show reset day-of-week and omit :00 from round hours
- Skip OAuth API call for proxies and non-Claude models
- Add hook-based context_window support for proxy models
- Hide window/reset for non-Claude proxy models
- Add beads issue tracker integration
- Add Gas Town multi-agent orchestration support
- Normalize raw model IDs into friendly display names

### Performance

- Replace globwalk with walkdir for 17x speedup

### Refactoring

- Reorganize monolithic main.rs into module structure
- Remove trailing .0 from whole number token formats
- Remove legacy plan tier system, use OAuth usage data
- Calculate percentage against full context limit
- Clean up symbols and separators
- Remove explanatory comments
- Remove all implementation reference comments
- Remove trailing decimals when zero for percentages
- Remove cc-sessions integration
- Remove user-agent impersonation and version detection
