#[derive(Default, Debug, Clone)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub short_commit: Option<String>,
    pub is_clean: Option<bool>,
    pub ahead: Option<usize>,
    pub behind: Option<usize>,
    pub remote_url: Option<String>,
    pub is_head_on_remote: Option<bool>,
    pub worktree_count: Option<usize>,
    pub is_linked_worktree: Option<bool>,
}
