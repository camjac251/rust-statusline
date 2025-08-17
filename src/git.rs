//! # Git Module  
//!
//! Provides Git repository inspection functionality using the gix library.
//!
//! ## Features
//!
//! - Branch and commit information
//! - Clean/dirty status detection
//! - Ahead/behind calculation against upstream
//! - Worktree detection
//! - Remote URL extraction

use crate::models::git::GitInfo;
use std::path::Path;

/// Maximum number of commits to walk when calculating ahead/behind
/// This prevents excessive CPU usage on large repositories
const MAX_ANCESTOR_WALK: usize = 10_000;

pub fn read_git_info(start_dir: &Path) -> Option<GitInfo> {
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
        if let (Some(remote), Some(merge_ref)) = (
            cfg.string(key_remote.as_str()),
            cfg.string(key_merge.as_str()),
        ) {
            let remote_s = remote.to_string();
            let merge_s = merge_ref.to_string();
            let merge_short = merge_s
                .strip_prefix("refs/heads/")
                .unwrap_or(merge_s.as_str());
            let upstream_ref = format!("refs/remotes/{}/{}", remote_s, merge_short);
            if let Ok(mut up_ref) = repo.find_reference(upstream_ref.as_str()) {
                if let Ok(up_id) = up_ref.peel_to_id_in_place() {
                    if let Ok(Some(head_id)) = repo.head().ok()?.try_peel_to_id_in_place() {
                        let mut head_set = std::collections::HashSet::<String>::new();
                        if let Ok(iter) = head_id.ancestors().all() {
                            for item in iter.flatten() {
                                head_set.insert(item.id.to_string());
                                if head_set.len() >= MAX_ANCESTOR_WALK {
                                    break;
                                }
                            }
                        }
                        let mut up_set = std::collections::HashSet::<String>::new();
                        if let Ok(iter) = up_id.ancestors().all() {
                            for item in iter.flatten() {
                                up_set.insert(item.id.to_string());
                                if up_set.len() >= MAX_ANCESTOR_WALK {
                                    break;
                                }
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
