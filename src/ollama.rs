use anyhow::{Context, Result};
use futures_util::TryStreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;
use tokio_util::io::StreamReader;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: ToolCallFunction,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize)]
struct ChatChunk {
    message: Option<ChunkMsg>,
    done: bool,
}

#[derive(Deserialize)]
struct ChunkMsg {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatResponseMsg,
}

#[derive(Deserialize)]
struct ChatResponseMsg {
    content: String,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<ModelInfo>,
}

#[derive(Deserialize)]
struct ModelInfo {
    name: String,
}

fn strip_model_artifacts(s: &str) -> String {
    const ARTIFACTS: &[&str] = &[
        "<|im_start|>", "<|im_end|>", "<|im_sep|>",
        "<|endoftext|>", "<|eot_id|>",
        "<|start_header_id|>", "<|end_header_id|>",
    ];
    let mut out = s.to_string();
    for token in ARTIFACTS {
        out = out.replace(token, "");
    }
    out
}

pub struct OllamaClient {
    client: Client,
    base_url: String,
}

impl OllamaClient {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
        }
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/tags", self.base_url);
        let resp: TagsResponse = self
            .client
            .get(&url)
            .send()
            .await
            .context("connecting to Ollama — is it running?")?
            .json()
            .await
            .context("parsing /api/tags response")?;
        Ok(resp.models.into_iter().map(|m| m.name).collect())
    }

    pub async fn chat_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        num_ctx: u32,
        mut on_chunk: impl FnMut(&str) -> std::ops::ControlFlow<(), ()>,
    ) -> Result<(String, Vec<ToolCall>)> {
        let url = format!("{}/api/chat", self.base_url);

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "options": {
                "num_ctx": num_ctx,
            },
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools).context("serializing tools")?;
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("sending chat request to Ollama")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Ollama returned {}: {}", status, text);
        }

        let stream = resp
            .bytes_stream()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
        let reader = StreamReader::new(stream);
        let buf_reader = tokio::io::BufReader::new(reader);
        let mut lines = buf_reader.lines();

        let mut full = String::new();
        let mut all_tool_calls: Vec<ToolCall> = Vec::new();

        while let Some(line) = lines.next_line().await? {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ChatChunk>(&line) {
                Ok(chunk) => {
                    if let Some(msg) = chunk.message {
                        if !msg.content.is_empty() {
                            let content = strip_model_artifacts(&msg.content);
                            if !content.is_empty() {
                                full.push_str(&content);
                                if let std::ops::ControlFlow::Break(_) = on_chunk(&content) {
                                    break;
                                }
                            }
                        }
                        if !msg.tool_calls.is_empty() {
                            all_tool_calls.extend(msg.tool_calls);
                        }
                    }
                    if chunk.done {
                        break;
                    }
                }
                Err(_) => {}
            }
        }

        Ok((full, all_tool_calls))
    }

    pub async fn chat_json(
        &self,
        model: &str,
        messages: &[ChatMessage],
    ) -> Result<String> {
        let url = format!("{}/api/chat", self.base_url);
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
            "format": "json",
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("sending chat_json request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Ollama returned {}: {}", status, text);
        }

        let r: ChatResponse = resp.json().await.context("parsing chat_json response")?;
        Ok(r.message.content)
    }
}
