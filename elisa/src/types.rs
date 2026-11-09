//! Shared message types (Anthropic Messages API shape). Owned by the coordinator; change only via DESIGN.md.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role { User, Assistant }
impl Default for Role { fn default() -> Self { Role::User } }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    /// a picture the user attached (only to a model with vision). Anthropic's shape; `openai.rs` turns it into an
    /// `image_url` data URL.
    Image { source: ImageSource },
}

/// `{"type": "base64", "media_type": "image/png", "data": "<base64>"}`
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub kind: String,
    pub media_type: String,
    pub data: String,
}

impl Block {
    pub fn image(media_type: &str, data: &str) -> Block {
        Block::Image { source: ImageSource { kind: "base64".into(), media_type: media_type.into(), data: data.into() } }
    }
}

/// the picture types every vision API takes
pub const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/// at most this many pictures in one message, each at most this many bytes of base64 (about 3.75 MB of image)
pub const IMAGES_MAX: usize = 4;
pub const IMAGE_B64_MAX: usize = 5_000_000;

/// `id` is local bookkeeping (the gateway's message id of an assistant reply, for its audit): kept in the session file,
/// never sent to the API (`api.rs` sends only role + content). Unknown keys (an old `vote`) are ignored on load.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// the model that wrote an assistant message (bug #66), never sent to the API. None: written in this run (stamped
    /// with the session's model on save); "" = unknown (a session saved before models were recorded)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: Vec<Block>) -> Self { Message { role, content, id: None, model: None } }
    pub fn user_text(t: impl Into<String>) -> Self { Message::new(Role::User, vec![Block::Text { text: t.into() }]) }
    pub fn assistant_text(t: impl Into<String>) -> Self { Message::new(Role::Assistant, vec![Block::Text { text: t.into() }]) }
}

/// Tool definition as sent in the request's `tools` array.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDef { pub name: String, pub description: String, pub input_schema: Value }

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input_tokens: usize,
    pub output_tokens: usize,
    /// prompt tokens the server reused from its KV / snapshot (streamdense gateway reports this)
    #[serde(default)]
    pub cache_read_input_tokens: usize,
    /// of `output_tokens`, the model's reasoning: generated (it counts for speed) but never kept in the history, so
    /// not in the window afterwards. Ollama's native API reports it; 0 elsewhere.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reasoning_tokens: usize,
}

/// One finished model reply.
#[derive(Clone, Debug, Default)]
pub struct Reply {
    /// the gateway's message id (`message_start.message.id`), empty when the server sends none
    pub id: String,
    pub content: Vec<Block>,
    /// end_turn | tool_use | max_tokens | stop_sequence | error
    pub stop_reason: String,
    pub usage: Usage,
    /// seconds from request send to first text/tool delta
    pub ttft_s: f64,
    pub total_s: f64,
}

fn is_zero(n: &usize) -> bool { *n == 0 }
