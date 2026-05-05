use anyhow::{Context, Result};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct CodeBlock {
    pub lang: String,
    pub code: String,
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

pub fn extract_blocks(text: &str) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    let mut in_fence = false;
    let mut lang = String::new();
    let mut code_lines: Vec<&str> = Vec::new();

    for line in text.lines() {
        if !in_fence {
            if let Some(rest) = line.trim_start().strip_prefix("```") {
                in_fence = true;
                lang = rest.trim().to_lowercase();
                code_lines.clear();
            }
        } else if line.trim_start().starts_with("```") {
            blocks.push(CodeBlock {
                lang: lang.clone(),
                code: code_lines.join("\n"),
            });
            in_fence = false;
            lang.clear();
            code_lines.clear();
        } else {
            code_lines.push(line);
        }
    }

    blocks
}

pub fn is_known_lang(lang: &str) -> bool {
    matches!(
        lang,
        "bash" | "sh" | "shell" | "python" | "py" | "python3" | "rust" | "rs" | "edit"
    )
}

/// Extract the target file path from an edit block (for confirmation prompts).
pub fn edit_target(code: &str) -> Option<String> {
    code.lines()
        .next()
        .and_then(|l| l.strip_prefix("file:"))
        .map(|p| p.trim().to_string())
}

pub async fn execute(block: &CodeBlock, timeout: Duration) -> Result<ExecResult> {
    match block.lang.as_str() {
        "bash" | "sh" | "shell" => {
            let mut cmd = Command::new("/bin/sh");
            cmd.arg("-c").arg(&block.code);
            run_with_timeout(cmd, timeout).await
        }
        "python" | "py" | "python3" => {
            let mut tmp = tempfile::Builder::new().suffix(".py").tempfile()?;
            tmp.write_all(block.code.as_bytes())?;
            tmp.flush()?;
            let path = tmp.path().to_owned();
            let mut cmd = Command::new("python3");
            cmd.arg(&path).stdin(std::process::Stdio::null());
            let result = run_with_timeout(cmd, timeout).await?;
            drop(tmp);
            Ok(result)
        }
        "rust" | "rs" => {
            let mut tmp_src = tempfile::Builder::new().suffix(".rs").tempfile()?;
            tmp_src.write_all(block.code.as_bytes())?;
            tmp_src.flush()?;
            let src_path = tmp_src.path().to_owned();
            let bin_path = src_path.with_extension("");

            let compile_script = format!(
                "rustc '{}' -o '{}'",
                src_path.display(),
                bin_path.display()
            );
            let mut compile_cmd = Command::new("/bin/sh");
            compile_cmd.arg("-c").arg(&compile_script);
            let compile_result = run_with_timeout(compile_cmd, timeout).await?;

            if compile_result.timed_out || compile_result.exit_code != 0 {
                drop(tmp_src);
                return Ok(compile_result);
            }

            let mut run_cmd = Command::new(&bin_path);
            run_cmd.stdin(std::process::Stdio::null());
            let run_result = run_with_timeout(run_cmd, timeout).await?;
            drop(tmp_src);
            let _ = std::fs::remove_file(&bin_path);
            Ok(run_result)
        }
        "edit" => apply_edit(&block.code),
        _ => anyhow::bail!("unknown language: {}", block.lang),
    }
}

// ---------------------------------------------------------------------------
// Edit block: surgical search-and-replace
//
// Format inside the fence:
//   file: path/to/file
//   <<<
//   exact text to find
//   ===
//   replacement text
//   >>>
// ---------------------------------------------------------------------------

fn parse_edit(code: &str) -> Result<(PathBuf, String, String)> {
    #[derive(PartialEq)]
    enum State { Header, Old, New }

    let mut state = State::Header;
    let mut path = PathBuf::new();
    let mut old_lines: Vec<&str> = Vec::new();
    let mut new_lines: Vec<&str> = Vec::new();

    for line in code.lines() {
        let trimmed = line.trim();
        match state {
            State::Header => {
                if let Some(p) = line.strip_prefix("file:") {
                    path = PathBuf::from(p.trim());
                } else if trimmed.starts_with("<<<") {
                    // accept  <<<  or  <<<<<<< SEARCH
                    state = State::Old;
                }
            }
            State::Old => {
                if trimmed.starts_with("===") || trimmed.starts_with("---") {
                    // accept  =======  or  ===
                    state = State::New;
                } else {
                    old_lines.push(line);
                }
            }
            State::New => {
                if trimmed.starts_with(">>>") {
                    // accept  >>>  or  >>>>>>> REPLACE
                    break;
                }
                new_lines.push(line);
            }
        }
    }

    if path.as_os_str().is_empty() {
        anyhow::bail!("edit block missing 'file: <path>'");
    }
    if state == State::Header {
        anyhow::bail!("edit block missing <<< marker");
    }

    Ok((path, old_lines.join("\n"), new_lines.join("\n")))
}

fn apply_edit(code: &str) -> Result<ExecResult> {
    let (path, old, new) = parse_edit(code)?;

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;

    if !content.contains(&old) {
        return Ok(ExecResult {
            stdout: String::new(),
            stderr: format!(
                "SEARCH text not found in {}.\nMake sure the text matches exactly (including indentation and whitespace).",
                path.display()
            ),
            exit_code: 1,
            timed_out: false,
        });
    }

    let updated = content.replacen(&old, &new, 1);
    std::fs::write(&path, &updated)
        .with_context(|| format!("writing {}", path.display()))?;

    Ok(ExecResult {
        stdout: format!("Edited {} successfully.", path.display()),
        stderr: String::new(),
        exit_code: 0,
        timed_out: false,
    })
}

async fn run_with_timeout(mut cmd: Command, dur: Duration) -> Result<ExecResult> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let stdout_handle = child.stdout.take().expect("piped");
    let stderr_handle = child.stderr.take().expect("piped");

    let stdout_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let mut h = stdout_handle;
        h.read_to_end(&mut buf).await.ok();
        buf
    });
    let stderr_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let mut h = stderr_handle;
        h.read_to_end(&mut buf).await.ok();
        buf
    });

    match tokio::time::timeout(dur, child.wait()).await {
        Ok(Ok(status)) => {
            let stdout = stdout_task.await.unwrap_or_default();
            let stderr = stderr_task.await.unwrap_or_default();
            Ok(ExecResult {
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
                exit_code: status.code().unwrap_or(-1),
                timed_out: false,
            })
        }
        Ok(Err(e)) => Err(e.into()),
        Err(_) => {
            child.kill().await.ok();
            stdout_task.abort();
            stderr_task.abort();
            Ok(ExecResult {
                stdout: String::new(),
                stderr: format!("timed out after {}s", dur.as_secs()),
                exit_code: -1,
                timed_out: true,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_no_blocks() {
        let text = "Hello world, no code here.";
        assert!(extract_blocks(text).is_empty());
    }

    #[test]
    fn test_extract_single_bash() {
        let text = "Run this:\n```bash\nls -la\n```\nDone.";
        let blocks = extract_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "bash");
        assert_eq!(blocks[0].code, "ls -la");
    }

    #[test]
    fn test_extract_multiple_blocks() {
        let text = "First:\n```bash\necho hi\n```\nSecond:\n```python\nprint('hello')\n```";
        let blocks = extract_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].lang, "bash");
        assert_eq!(blocks[1].lang, "python");
    }

    #[test]
    fn test_extract_unknown_lang() {
        let text = "```javascript\nconsole.log('hi');\n```";
        let blocks = extract_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "javascript");
        assert!(!is_known_lang("javascript"));
    }

    #[test]
    fn test_extract_no_lang() {
        let text = "```\nsome output\n```";
        let blocks = extract_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "");
        assert!(!is_known_lang(""));
    }

    #[test]
    fn test_inline_backticks_ignored() {
        let text = "Use `ls` to list files. No fenced blocks here.";
        assert!(extract_blocks(text).is_empty());
    }

    #[test]
    fn test_multiline_code() {
        let text = "```bash\ncd /tmp\nls -la\necho done\n```";
        let blocks = extract_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code, "cd /tmp\nls -la\necho done");
    }

    #[test]
    fn test_parse_edit_block() {
        let code = "file: hello.py\n<<<\nprint(\"Hello\")\n===\nprint(\"Hi\")\n>>>";
        let (path, old, new) = parse_edit(code).unwrap();
        assert_eq!(path, PathBuf::from("hello.py"));
        assert_eq!(old, "print(\"Hello\")");
        assert_eq!(new, "print(\"Hi\")");
    }

    #[test]
    fn test_parse_edit_multiline() {
        let code = "file: app.py\n<<<\ndef foo():\n    pass\n===\ndef foo():\n    return 42\n>>>";
        let (path, old, new) = parse_edit(code).unwrap();
        assert_eq!(path, PathBuf::from("app.py"));
        assert_eq!(old, "def foo():\n    pass");
        assert_eq!(new, "def foo():\n    return 42");
    }

    #[test]
    fn test_edit_target() {
        let code = "file: src/main.py\n<<<\nold\n===\nnew\n>>>";
        assert_eq!(edit_target(code), Some("src/main.py".to_string()));
    }

    #[test]
    fn test_parse_edit_git_style() {
        let code = "file: app.py\n<<<<<<< SEARCH\nold text\n=======\nnew text\n>>>>>>> REPLACE";
        let (path, old, new) = parse_edit(code).unwrap();
        assert_eq!(path, PathBuf::from("app.py"));
        assert_eq!(old, "old text");
        assert_eq!(new, "new text");
    }

    #[test]
    fn test_parse_edit_space_before_separator() {
        // model sometimes emits " ===" with a leading space — should still work
        let code = "file: f.py\n<<<\nold\n ===\nnew\n>>>";
        let (path, old, new) = parse_edit(code).unwrap();
        assert_eq!(path, PathBuf::from("f.py"));
        assert_eq!(old, "old");
        assert_eq!(new, "new");
    }
}
