//! # Claude Statusline
//!
//! A lightweight statusline utility for Claude Code sessions that provides real-time
//! cost tracking, usage metrics, and Git context information.
//!
//! ## Overview
//!
//! This library processes Claude Code session data from JSON hooks and transcript files
//! to generate formatted output (text or JSON) showing:
//! - Session and daily cost tracking
//! - Token usage and burn rates
//! - 5-hour window usage tracking with projections
//! - Git repository status and branch information
//! - Context window utilization
//!
//! ## Features
//!
//! - `git` (default): Enables repository inspection via gix
//! - `colors` (default): Enables terminal color output via owo-colors

/// In-memory caching for parsed JSONL data
pub mod cache;

/// Command-line argument parsing and configuration
pub mod cli;

/// Display formatting for text and JSON output
pub mod display;

/// Git repository inspection (feature-gated)
#[cfg(feature = "git")]
pub mod git;

/// Data models for hooks, entries, blocks, and Git info
pub mod models;

/// Model-specific pricing calculations
pub mod pricing;

/// cc-sessions integration
pub mod sessions;

/// Usage tracking and block identification
pub mod usage;

/// Online usage limits retrieved from the Claude OAuth API
pub mod usage_api;

/// Utility functions for paths, formatting, and time
pub mod utils;

/// Window calculation and metrics
pub mod window;

// OAuth helpers removed for offline-only mode
