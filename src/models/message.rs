use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct MessageUsage {
    pub input_tokens: Option<u64>,
    #[allow(dead_code)]
    pub output_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Deserialize, Debug)]
pub struct MessageObj {
    pub usage: Option<MessageUsage>,
}

#[derive(Deserialize, Debug)]
pub struct TranscriptLine {
    pub r#type: Option<String>,
    pub message: Option<MessageObj>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct UsageLineMessage {
    pub usage: MessageUsage,
    pub model: Option<String>,
}
