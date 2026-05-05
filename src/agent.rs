use anyhow::{Context, Result};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use crate::skills::{self, Skill};

#[derive(serde::Deserialize)]
struct SkillRouteResponse {
    selected_skills: Vec<String>,
}

use crate::cli::Args;
use crate::ollama::{ChatMessage, OllamaClient};
use crate::prompt::SYSTEM_PROMPT;
use crate::terminal;
use crate::tools;

pub enum AgentMsg {
    Chunk(String),
    ClearLastText,
    ToolProposed { name: String, args_display: String, confirm_tx: oneshot::Sender<bool> },
    ToolAutoRan { name: String, args_display: String },
    ToolResult { name: String, content: String, is_error: bool },
    TurnComplete,
    Error(String),
}

pub struct Agent {
    pub client: OllamaClient,
    pub model: String,
    history: Vec<ChatMessage>,
    pub no_confirm: bool,
    exec_timeout: Duration,
    pub pending_images: Vec<String>,
    system_prompt_base: String,
    available_skills: Vec<Skill>,
    current_goal: Option<String>,
}

impl Agent {
    pub async fn new(args: &Args) -> Result<Self> {
        let client = OllamaClient::new(args.url.clone());

        let model = match &args.model {
            Some(m) => m.clone(),
            None => {
                let models = client
                    .list_models()
                    .await
                    .context("listing Ollama models — is Ollama running?")?;
                models
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("no Ollama models installed — run `ollama pull <model>`"))?
            }
        };

        let system_prompt_base = if let Some(path) = &args.system {
            std::fs::read_to_string(path)
                .with_context(|| format!("reading system prompt from {}", path.display()))?
        } else {
            SYSTEM_PROMPT.to_string()
        };

        let available_skills = skills::load_skills().unwrap_or_default();

        let history = vec![ChatMessage {
            role: "system".into(),
            content: system_prompt_base.clone(),
            images: None,
            tool_calls: None,
        }];

        Ok(Self {
            client,
            model,
            history,
            no_confirm: args.no_confirm,
            exec_timeout: Duration::from_secs(args.timeout),
            pending_images: Vec::new(),
            system_prompt_base,
            available_skills,
            current_goal: None,
        })
    }

    async fn route_skills(&self, user_input: &str) -> Vec<Skill> {
        if self.available_skills.is_empty() { return vec![]; }

        let mut skill_names = String::new();
        for s in &self.available_skills {
            skill_names.push_str(&format!("- {}: {}\n", s.name, s.description));
        }

        let router_prompt = format!(
            "You are a routing agent. Below is the user's latest query.\n\
             USER: {}\n\n\
             Here are the available skills:\n{}\n\
             Return ONLY a JSON object with a single key `selected_skills` containing an array of strings representing the names of up to 4 skills the user needs to fulfill this request. \
             If no skills are needed, return an empty array.",
            user_input, skill_names
        );

        let msgs = vec![ChatMessage {
            role: "user".into(),
            content: router_prompt,
            images: None,
            tool_calls: None,
        }];

        if let Ok(res) = self.client.chat_json(&self.model, &msgs).await {
            if let Ok(data) = serde_json::from_str::<SkillRouteResponse>(&res) {
                return self.available_skills.iter().filter(|s| data.selected_skills.contains(&s.name)).cloned().collect();
            }
        }
        vec![]
    }

    fn rebuild_system_prompt(&mut self, selected_skills: &[Skill]) {
        let mut full = self.system_prompt_base.clone();

        if let Some(goal) = &self.current_goal {
            full.push_str("\n\n=== CURRENT GOAL ===\n> ");
            full.push_str(goal);
            full.push_str("\n====================\n");
        }

        if !selected_skills.is_empty() {
            full.push_str("\n\n=== INJECTED SKILLS ===\n");
            for s in selected_skills {
                full.push_str(&format!("\n# {}\n{}\n", s.name, s.instructions));
            }
            full.push_str("=======================\n");
        }

        if let Some(sys) = self.history.first_mut() {
            sys.content = full;
        }
    }

    fn resolve_tool_calls(
        full: &str,
        native: Vec<crate::ollama::ToolCall>,
    ) -> Vec<crate::ollama::ToolCall> {
        if !native.is_empty() {
            return native;
        }

        let known = ["write_file", "read_file", "run_bash", "apply_edit"];

        for v in Self::json_candidates(full) {
            if let Some(tc) = Self::parse_tool_call_value(&v, &known) {
                return vec![tc];
            }
        }

        vec![]
    }

    /// Extract candidate JSON values from code fences first, then anywhere in text.
    /// Uses a streaming deserializer so trailing characters (e.g. stray `}`) are ignored.
    fn json_candidates(text: &str) -> Vec<serde_json::Value> {
        use serde::de::Deserialize as _;

        fn first_json(s: &str) -> Option<serde_json::Value> {
            for (i, c) in s.char_indices() {
                if c == '{' {
                    let mut de = serde_json::Deserializer::from_str(&s[i..]);
                    if let Ok(v) = serde_json::Value::deserialize(&mut de) {
                        return Some(v);
                    }
                }
            }
            None
        }

        let mut candidates = Vec::new();

        // Scan all code fences anywhere in the response
        let mut in_fence = false;
        let mut fence_lines: Vec<&str> = Vec::new();
        for line in text.lines() {
            if !in_fence {
                if line.trim_start().starts_with("```") {
                    in_fence = true;
                    fence_lines.clear();
                }
            } else if line.trim_start().starts_with("```") {
                if let Some(v) = first_json(&fence_lines.join("\n")) {
                    candidates.push(v);
                }
                in_fence = false;
            } else {
                fence_lines.push(line);
            }
        }

        // Fallback: find first valid JSON object anywhere in the full text
        if candidates.is_empty() {
            if let Some(v) = first_json(text) {
                candidates.push(v);
            }
        }

        candidates
    }

    fn parse_tool_call_value(
        v: &serde_json::Value,
        known: &[&str],
    ) -> Option<crate::ollama::ToolCall> {
        // Format: {"name": "...", "arguments": {...}}
        if let (Some(name), Some(args)) = (v["name"].as_str(), v.get("arguments")) {
            if known.contains(&name) {
                return Some(crate::ollama::ToolCall {
                    function: crate::ollama::ToolCallFunction {
                        name: name.to_string(),
                        arguments: args.clone(),
                    },
                });
            }
        }

        // Format: {"function": {"name": "...", "arguments": {...}}}
        if let Some(func) = v.get("function") {
            if let (Some(name), Some(args)) = (func["name"].as_str(), func.get("arguments")) {
                if known.contains(&name) {
                    return Some(crate::ollama::ToolCall {
                        function: crate::ollama::ToolCallFunction {
                            name: name.to_string(),
                            arguments: args.clone(),
                        },
                    });
                }
            }
        }

        // Format: {"tool": "...", "args": {...}}
        if let (Some(name), Some(args)) = (v["tool"].as_str(), v.get("args")) {
            if known.contains(&name) {
                return Some(crate::ollama::ToolCall {
                    function: crate::ollama::ToolCallFunction {
                        name: name.to_string(),
                        arguments: args.clone(),
                    },
                });
            }
        }

        None
    }

    pub async fn turn(&mut self, user_input: String) -> Result<()> {
        let images = if self.pending_images.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending_images))
        };

        self.current_goal = Some(user_input.clone());
        let selected_skills = self.route_skills(&user_input).await;
        self.rebuild_system_prompt(&selected_skills);

        self.history.push(ChatMessage {
            role: "user".into(),
            content: user_input,
            images,
            tool_calls: None,
        });

        let tool_defs = tools::tool_definitions();

        loop {
            terminal::print_assistant_label();
            terminal::print_thinking();

            let mut first_chunk = true;
            let res = self
                .client
                .chat_stream(&self.model, &self.history, &tool_defs, 16384, |chunk| {
                    if first_chunk {
                        terminal::clear_thinking();
                        first_chunk = false;
                    }
                    terminal::stream_chunk(chunk);
                    std::ops::ControlFlow::Continue(())
                })
                .await;

            if first_chunk {
                terminal::clear_thinking();
            }

            let (full, native_calls) = res?;
            let used_text_fallback = native_calls.is_empty();
            let tool_calls = Self::resolve_tool_calls(&full, native_calls);
            let text_was_tool_call = used_text_fallback && !tool_calls.is_empty();

            self.history.push(ChatMessage {
                role: "assistant".into(),
                content: if text_was_tool_call { String::new() } else { full.clone() },
                images: None,
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls.clone()) },
            });

            if tool_calls.is_empty() {
                println!();
                return Ok(());
            }

            println!();

            // Execute tool calls
            for call in &tool_calls {
                let name = &call.function.name;
                let args = &call.function.arguments;
                let args_display = serde_json::to_string_pretty(args).unwrap_or_default();

                terminal::print_tool_call(name, &args_display);

                let confirmed = if self.no_confirm {
                    true
                } else {
                    terminal::confirm(&format!("call {}?", name))
                };

                if !confirmed {
                    self.history.push(ChatMessage {
                        role: "tool".into(),
                        content: "[User declined this tool call]".to_string(),
                        images: None,
                        tool_calls: None,
                    });
                    return Ok(());
                }

                let result = tools::dispatch(name, args, self.exec_timeout).await;
                terminal::print_tool_result(&result.content, result.is_error);

                self.history.push(ChatMessage {
                    role: "tool".into(),
                    content: result.content,
                    images: None,
                    tool_calls: None,
                });
            }
        }
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        self.client.list_models().await
    }

    pub async fn turn_tui(
        &mut self,
        user_input: String,
        tx: mpsc::UnboundedSender<AgentMsg>,
    ) -> Result<()> {
        let images = if self.pending_images.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending_images))
        };

        self.current_goal = Some(user_input.clone());
        let selected_skills = self.route_skills(&user_input).await;
        self.rebuild_system_prompt(&selected_skills);

        self.history.push(ChatMessage {
            role: "user".into(),
            content: user_input,
            images,
            tool_calls: None,
        });

        let tool_defs = tools::tool_definitions();

        loop {
            let tx2 = tx.clone();

            let res = self.client.chat_stream(
                &self.model,
                &self.history,
                &tool_defs,
                16384,
                |chunk| {
                    let _ = tx2.send(AgentMsg::Chunk(chunk.to_string()));
                    std::ops::ControlFlow::Continue(())
                },
            ).await;

            let (full, native_calls) = res?;
            let used_text_fallback = native_calls.is_empty();
            let tool_calls = Self::resolve_tool_calls(&full, native_calls);
            let text_was_tool_call = used_text_fallback && !tool_calls.is_empty();

            // If model output raw JSON as text, clear it from the TUI before showing the tool box
            if text_was_tool_call {
                let _ = tx.send(AgentMsg::ClearLastText);
            }

            self.history.push(ChatMessage {
                role: "assistant".into(),
                content: if text_was_tool_call { String::new() } else { full.clone() },
                images: None,
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls.clone()) },
            });

            if tool_calls.is_empty() {
                let _ = tx.send(AgentMsg::TurnComplete);
                return Ok(());
            }

            // Process each tool call
            for call in &tool_calls {
                let name = call.function.name.clone();
                let args = call.function.arguments.clone();
                let args_display = serde_json::to_string_pretty(&args).unwrap_or_default();

                let confirmed = if self.no_confirm {
                    let _ = tx.send(AgentMsg::ToolAutoRan {
                        name: name.clone(),
                        args_display: args_display.clone(),
                    });
                    true
                } else {
                    let (confirm_tx, confirm_rx) = oneshot::channel();
                    let _ = tx.send(AgentMsg::ToolProposed {
                        name: name.clone(),
                        args_display: args_display.clone(),
                        confirm_tx,
                    });
                    match confirm_rx.await {
                        Ok(v) => v,
                        Err(_) => return Ok(()),
                    }
                };

                if !confirmed {
                    self.history.push(ChatMessage {
                        role: "tool".into(),
                        content: "[User declined this tool call]".to_string(),
                        images: None,
                        tool_calls: None,
                    });
                    let _ = tx.send(AgentMsg::ToolResult {
                        name: name.clone(),
                        content: "Declined by user.".to_string(),
                        is_error: false,
                    });
                    let _ = tx.send(AgentMsg::TurnComplete);
                    return Ok(());
                }

                let result = tools::dispatch(&name, &args, self.exec_timeout).await;

                self.history.push(ChatMessage {
                    role: "tool".into(),
                    content: result.content.clone(),
                    images: None,
                    tool_calls: None,
                });

                let _ = tx.send(AgentMsg::ToolResult {
                    name: name.clone(),
                    content: result.content,
                    is_error: result.is_error,
                });
            }
            // Loop: model may call more tools or give a final text response
        }
    }
}
