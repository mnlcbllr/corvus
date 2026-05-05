use anyhow::{Context, Result};
use futures_util::TryStreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;
use tokio_util::io::StreamReader;

#[derive(Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ChatChunk {
    message: Option<ChunkMsg>,
    done: bool,
}

#[derive(Deserialize)]
struct ChunkMsg {
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChunkMsg,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<ModelInfo>,
}

#[derive(Deserialize)]
struct ModelInfo {
    name: String,
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
        mut on_chunk: impl FnMut(&str) -> std::ops::ControlFlow<(), ()>,
    ) -> Result<String> {
        let url = format!("{}/api/chat", self.base_url);
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
        });

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
        while let Some(line) = lines.next_line().await? {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ChatChunk>(&line) {
                Ok(chunk) => {
                    if let Some(msg) = chunk.message {
                        full.push_str(&msg.content);
                        if let std::ops::ControlFlow::Break(_) = on_chunk(&msg.content) {
                            break;
                        }
                    }
                    if chunk.done {
                        break;
                    }
                }
                Err(_) => {
                    // Non-JSON line (unlikely with Ollama but ignore gracefully)
                }
            }
        }

        Ok(full)
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
            "format": "json"
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("sending json chat request")?;

        if !resp.status().is_success() {
            anyhow::bail!("Ollama returned {}", resp.status());
        }

        let data: ChatResponse = resp.json().await?;
        Ok(data.message.content)
    }
}
