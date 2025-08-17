use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct HookModel {
    pub id: String,
    pub display_name: String,
}

#[derive(Deserialize, Debug)]
pub struct HookWorkspace {
    pub current_dir: String,
    pub project_dir: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct OutputStyle {
    pub name: String,
}

#[derive(Deserialize, Debug)]
pub struct HookJson {
    pub session_id: String,
    pub transcript_path: String,
    #[allow(dead_code)]
    pub cwd: Option<String>,
    pub model: HookModel,
    pub workspace: HookWorkspace,
    pub version: Option<String>,
    pub output_style: Option<OutputStyle>,
}
