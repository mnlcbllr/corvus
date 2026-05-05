use anyhow::{Context, Result};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use crate::skills::{self, Skill};

#[derive(serde::Deserialize)]
struct SkillRouteResponse {
    selected_skills: Vec<String>,
}

use crate::cli::Args;
use crate::executor::{self, CodeBlock, ExecResult};
use crate::ollama::{ChatMessage, OllamaClient};
use crate::prompt::SYSTEM_PROMPT;
use crate::terminal;

pub enum AgentMsg {
    Chunk(String),
    BlockProposed { block: CodeBlock, confirm_tx: oneshot::Sender<bool> },
    BlockAutoRan(CodeBlock),
    BlockResult { result: ExecResult },
    TurnComplete,
    Error(String),
}

struct StreamFilter {
    buffer: String,
    in_code_block: bool,
    is_output_block: bool,
}

impl StreamFilter {
    fn new() -> Self {
        Self { buffer: String::new(), in_code_block: false, is_output_block: false }
    }
    fn process(&mut self, chunk: &str, mut print: impl FnMut(&str)) -> std::ops::ControlFlow<(), ()> {
        print(chunk);
        self.buffer.push_str(chunk);
        while let Some(idx) = self.buffer.find('\n') {
            let line = self.buffer[..=idx].to_string();
            self.buffer.drain(..=idx);
            
            if line.trim_start().starts_with("```") {
                if self.in_code_block {
                    self.in_code_block = false; 
                    let was_output = self.is_output_block;
                    self.is_output_block = false;
                    
                    if !was_output {
                        return std::ops::ControlFlow::Break(());
                    }
                } else {
                    self.in_code_block = true;
                    let lang = line.trim_start().strip_prefix("```").unwrap_or("").trim().to_lowercase();
                    self.is_output_block = lang == "output";
                }
            }
        }
        std::ops::ControlFlow::Continue(())
    }
    fn flush(&mut self, _print: impl FnMut(&str)) {
        self.buffer.clear();
    }
}

pub struct Agent {
    pub client: OllamaClient,
    pub model: String,
    history: Vec<ChatMessage>,
    pub no_confirm: bool,
    exec_timeout: Duration,
    last_declined_code: Option<String>,
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
        }];

        Ok(Self {
            client,
            model,
            history,
            no_confirm: args.no_confirm,
            exec_timeout: Duration::from_secs(args.timeout),
            last_declined_code: None,
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
            full.push_str("\n\n=== MULTI-STEP CONTEXT KEEP-ALIVE ===\n");
            full.push_str("To prevent goal amnesia during multi-step tracing, here is your overarching goal:\n> ");
            full.push_str(goal);
            full.push_str("\n====================================\n");
        }

        if !selected_skills.is_empty() {
            full.push_str("\n\n=== INJECTED DYNAMIC SKILLS FOR THIS TURN ===\n");
            for s in selected_skills {
                full.push_str(&format!("\n# {}\n{}\n", s.name, s.instructions));
            }
            full.push_str("==============================================\n");
        }

        if let Some(sys) = self.history.first_mut() {
            sys.content = full;
        }
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
        });

        loop {
            terminal::print_assistant_label();
            terminal::print_thinking();

            let mut first_chunk = true;
            let mut filter = StreamFilter::new();
            let res = self
                .client
                .chat_stream(&self.model, &self.history, |chunk| {
                    if first_chunk {
                        terminal::clear_thinking();
                        first_chunk = false;
                    }
                    filter.process(chunk, |s| terminal::stream_chunk(s))
                })
                .await;
            filter.flush(|s| terminal::stream_chunk(s));

            if first_chunk {
                terminal::clear_thinking();
            }

            let full = res?;

            self.history.push(ChatMessage {
                role: "assistant".into(),
                content: full.clone(),
                images: None,
            });

            let mut blocks = executor::extract_blocks(&full);
            blocks.retain(|b| b.lang != "output" && !b.lang.is_empty());
            if blocks.is_empty() {
                return Ok(());
            }

            let mut observation = String::new();
            for block in &blocks {
                let result = self.run_block(block).await?;
                observation.push_str(&format_observation(block, &result));

                if result.exit_code != 0 {
                    observation.push_str("\n[System] A command returned a non-zero error code. Execution of any remaining blocks in this response has been automatically aborted to prevent cascading failures. Please evaluate the error.\n");
                    break;
                }
            }

            self.history.push(ChatMessage {
                role: "user".into(),
                content: observation,
                images: None,
            });
        }
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        self.client.list_models().await
    }

    async fn run_block(&mut self, block: &CodeBlock) -> Result<ExecResult> {
        if let Some(declined) = &self.last_declined_code {
            if declined == &block.code {
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "duplicate declined block detected".into(),
                    exit_code: 130, 
                    timed_out: false,
                });
            }
        }

        if !executor::is_known_lang(&block.lang) {
            terminal::print_warning(&format!(
                "unknown code language '{}' — skipping execution",
                if block.lang.is_empty() {
                    "(none)"
                } else {
                    &block.lang
                }
            ));
            return Ok(ExecResult {
                stdout: String::new(),
                stderr: format!("skipped: unknown language '{}'", block.lang),
                exit_code: 1,
                timed_out: false,
            });
        }

        if block.lang == "edit" {
            terminal::print_edit_block(&block.code);
        } else {
            terminal::print_code_fence(&block.lang, &block.code);
        }

        if !self.no_confirm {
            let prompt = if block.lang == "edit" {
                let target = executor::edit_target(&block.code)
                    .unwrap_or_else(|| "file".into());
                format!("apply this edit to {}?", target)
            } else {
                let label = if block.lang.is_empty() { "shell" } else { &block.lang };
                format!("run this {} block?", label)
            };
            if !terminal::confirm(&prompt) {
                self.last_declined_code = Some(block.code.clone());
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: "user declined".into(),
                    exit_code: 130,
                    timed_out: false,
                });
            } else {
                self.last_declined_code = None;
            }
        } else {
            self.last_declined_code = None;
        }

        let result = executor::execute(block, self.exec_timeout).await?;
        terminal::print_exec_output(&result);
        Ok(result)
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
        });

        loop {
            let mut filter = StreamFilter::new();
            let tx2 = tx.clone();

            let res = self.client.chat_stream(&self.model, &self.history, |chunk| {
                filter.process(chunk, |text| {
                    let _ = tx2.send(AgentMsg::Chunk(text.to_string()));
                })
            }).await;
            filter.flush(|line| {
                let _ = tx2.send(AgentMsg::Chunk(line.to_string()));
            });

            let full = res?;

            self.history.push(ChatMessage {
                role: "assistant".into(),
                content: full.clone(),
                images: None,
            });

            let mut blocks = executor::extract_blocks(&full);
            blocks.retain(|b| b.lang != "output" && !b.lang.is_empty() && executor::is_known_lang(&b.lang));

            if blocks.is_empty() {
                let _ = tx.send(AgentMsg::TurnComplete);
                return Ok(());
            }

            let mut observation = String::new();
            let mut abort = false;

            for block in &blocks {
                let confirmed = if self.no_confirm {
                    let _ = tx.send(AgentMsg::BlockAutoRan(block.clone()));
                    true
                } else {
                    let (confirm_tx, confirm_rx) = oneshot::channel();
                    let _ = tx.send(AgentMsg::BlockProposed { block: block.clone(), confirm_tx });
                    match confirm_rx.await {
                        Ok(v) => v,
                        Err(_) => return Ok(()), // TUI dropped
                    }
                };

                if !confirmed {
                    self.last_declined_code = Some(block.code.clone());
                    let obs = format_observation(block, &ExecResult {
                        stdout: String::new(),
                        stderr: "user declined".into(),
                        exit_code: 130,
                        timed_out: false,
                    });
                    observation.push_str(&obs);
                    abort = true;
                    break;
                }

                self.last_declined_code = None;
                let result = executor::execute(block, self.exec_timeout).await?;
                let failed = result.exit_code != 0;
                observation.push_str(&format_observation(block, &result));
                let _ = tx.send(AgentMsg::BlockResult { result });

                if failed {
                    observation.push_str("\n[System] A command returned a non-zero error code. Remaining blocks aborted.\n");
                    abort = true;
                    break;
                }
            }

            self.history.push(ChatMessage {
                role: "user".into(),
                content: observation,
                images: None,
            });

            if abort {
                let _ = tx.send(AgentMsg::TurnComplete);
                return Ok(());
            }
        }
    }
}

fn format_observation(block: &CodeBlock, result: &ExecResult) -> String {
    let lang = if block.lang.is_empty() { "shell" } else { &block.lang };
    
    // Explicit system overrides to heavily guide smaller LLMs
    if result.exit_code == 130 && result.stderr.contains("duplicate declined block detected") {
        return format!("[System] FATAL: You just proposed the exact same code block that I ALREADY DECLINED. Stop looping and ask me what to do, or try a completely different approach.\n");
    } else if result.exit_code == 130 && result.stderr.contains("user declined") {
        return format!("[System] You presented a `{}` block, but the user explicitly declined to run it. DO NOT retry the exact same command. Ask the user what they want to do next.\n", lang);
    } else if result.exit_code == 1 && result.stderr.starts_with("skipped: unknown language") {
        return format!("[System] Error: `{}` is not a supported language for execution. Please use a valid executing block type.\n", lang);
    }

    let mut s = if result.timed_out {
        format!("Ran `{}` block [timed out after {}s]\n", lang, result.exit_code.unsigned_abs())
    } else {
        format!("Ran `{}` block [exit {}]\n", lang, result.exit_code)
    };

    if !result.stdout.is_empty() {
        s.push_str("stdout:\n```\n");
        let limit = 2000;
        let out = result.stdout.trim_end();
        if out.len() > limit {
            let safe_out: String = out.chars().take(limit).collect();
            s.push_str(&safe_out);
            s.push_str("\n\n... [SYSTEM WARNING: STDOUT TRUNCATED BEACUSE IT EXCEEDED TOKENS] ...\n... Use the `grep_search` skill to read this file carefully instead of dumping it.\n");
        } else {
            s.push_str(out);
        }
        s.push_str("\n```\n");
    }

    if !result.stderr.is_empty() {
        s.push_str("stderr:\n```\n");
        let limit = 2000;
        let err = result.stderr.trim_end();
        if err.len() > limit {
            let safe_err: String = err.chars().take(limit).collect();
            s.push_str(&safe_err);
            s.push_str("\n\n... [SYSTEM WARNING: STDERR TRUNCATED BEACUSE IT EXCEEDED TOKENS] ...\n");
        } else {
            s.push_str(err);
        }
        s.push_str("\n```\n");
    }

    if result.stdout.is_empty() && result.stderr.is_empty() {
        s.push_str("(no output)\n");
    }

    s
}
