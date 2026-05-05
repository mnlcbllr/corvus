use std::time::Duration;
use serde_json::Value;
use crate::ollama::{ToolDefinition, ToolFunction};

pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "write_file".into(),
                description: "Create or overwrite a file on disk with the given content.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "File path to write (e.g. 'diamond.py' or 'src/main.rs')"
                        },
                        "content": {
                            "type": "string",
                            "description": "Full file content to write"
                        }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "read_file".into(),
                description: "Read a file's contents. Call this before apply_edit to get exact text.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "File path to read"
                        },
                        "start_line": {
                            "type": "integer",
                            "description": "1-based line number to start reading from (optional)"
                        },
                        "end_line": {
                            "type": "integer",
                            "description": "1-based line number to stop reading at, inclusive (optional)"
                        }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "run_bash".into(),
                description: "Run a shell command. Use for: ls/find/grep exploration, running tests, building, executing scripts. Check exit code in the result.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "Shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "apply_edit".into(),
                description: "Surgically replace an exact string in an existing file. Call read_file first to get the exact text including indentation.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "File path to edit"
                        },
                        "search": {
                            "type": "string",
                            "description": "Exact text to find (must match exactly, including whitespace)"
                        },
                        "replace": {
                            "type": "string",
                            "description": "Text to replace it with"
                        }
                    },
                    "required": ["path", "search", "replace"]
                }),
            },
        },
    ]
}

pub async fn dispatch(name: &str, args: &Value, timeout: Duration) -> ToolResult {
    match name {
        "write_file" => {
            let path = match args["path"].as_str() {
                Some(p) => p,
                None => return ToolResult { content: "missing 'path' argument".into(), is_error: true },
            };
            let content = match args["content"].as_str() {
                Some(c) => c,
                None => return ToolResult { content: "missing 'content' argument".into(), is_error: true },
            };
            // Create parent dirs if needed
            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return ToolResult { content: format!("Error creating directories: {}", e), is_error: true };
                    }
                }
            }
            match std::fs::write(path, content) {
                Ok(_) => ToolResult {
                    content: format!("Written {} ({} bytes)", path, content.len()),
                    is_error: false,
                },
                Err(e) => ToolResult { content: format!("Error writing {}: {}", path, e), is_error: true },
            }
        }

        "read_file" => {
            let path = match args["path"].as_str() {
                Some(p) => p,
                None => return ToolResult { content: "missing 'path' argument".into(), is_error: true },
            };
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => return ToolResult { content: format!("Error reading {}: {}", path, e), is_error: true },
            };
            let lines: Vec<&str> = content.lines().collect();
            let start = args["start_line"]
                .as_u64()
                .map(|n| (n as usize).saturating_sub(1))
                .unwrap_or(0);
            let end = args["end_line"]
                .as_u64()
                .map(|n| (n as usize).min(lines.len()))
                .unwrap_or(lines.len());
            let slice = lines.get(start..end).unwrap_or(&[]).join("\n");
            ToolResult { content: slice, is_error: false }
        }

        "run_bash" => {
            let command = match args["command"].as_str() {
                Some(c) => c,
                None => return ToolResult { content: "missing 'command' argument".into(), is_error: true },
            };
            match crate::executor::run_bash(command, timeout).await {
                Ok(result) => {
                    let mut output = String::new();
                    if !result.stdout.is_empty() {
                        output.push_str(result.stdout.trim_end());
                    }
                    if !result.stderr.is_empty() {
                        if !output.is_empty() { output.push('\n'); }
                        output.push_str("stderr:\n");
                        output.push_str(result.stderr.trim_end());
                    }
                    if output.is_empty() { output.push_str("(no output)"); }
                    output.push_str(&format!("\n[exit {}]", result.exit_code));
                    ToolResult { content: output, is_error: result.exit_code != 0 }
                }
                Err(e) => ToolResult { content: format!("Failed to run command: {}", e), is_error: true },
            }
        }

        "apply_edit" => {
            let path = match args["path"].as_str() {
                Some(p) => p,
                None => return ToolResult { content: "missing 'path' argument".into(), is_error: true },
            };
            let search = match args["search"].as_str() {
                Some(s) => s,
                None => return ToolResult { content: "missing 'search' argument".into(), is_error: true },
            };
            let replace = match args["replace"].as_str() {
                Some(r) => r,
                None => return ToolResult { content: "missing 'replace' argument".into(), is_error: true },
            };
            match crate::executor::apply_edit_parts(path, search, replace) {
                Ok(result) => ToolResult {
                    content: if result.exit_code == 0 { result.stdout } else { result.stderr },
                    is_error: result.exit_code != 0,
                },
                Err(e) => ToolResult { content: format!("Error: {}", e), is_error: true },
            }
        }

        _ => ToolResult { content: format!("Unknown tool: {}", name), is_error: true },
    }
}
