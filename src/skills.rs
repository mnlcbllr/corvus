use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub instructions: String,
}

pub fn skills_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".corvus")
        .join("skills")
}

pub fn load_skills() -> Result<Vec<Skill>> {
    let dir = skills_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
        seed_default_skills(&dir).ok();
    }

    let mut skills = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|ext| ext == "md") {
            if let Ok(skill) = parse_markdown_skill(&entry.path()) {
                skills.push(skill);
            }
        }
    }
    Ok(skills)
}

// Parses a simple markdown file into a Skill where the title `# Name` is the name,
// `> description` block is the description, and everything else is instructions.
fn parse_markdown_skill(path: &Path) -> Result<Skill> {
    let content = fs::read_to_string(path)?;
    let mut name = String::new();
    let mut description = String::new();
    let mut instructions = String::new();

    let mut in_desc = false;
    for line in content.lines() {
        if line.starts_with("# ") && name.is_empty() {
            name = line.strip_prefix("# ").unwrap().trim().to_string();
        } else if line.starts_with("> ") && description.is_empty() {
            in_desc = true;
            description.push_str(line.strip_prefix("> ").unwrap().trim());
        } else if in_desc && line.starts_with("> ") {
            description.push(' ');
            description.push_str(line.strip_prefix("> ").unwrap().trim());
        } else if in_desc && line.is_empty() {
            in_desc = false;
        } else if !name.is_empty() && !in_desc {
            instructions.push_str(line);
            instructions.push('\n');
        }
    }

    if name.is_empty() {
        name = path.file_stem().unwrap().to_string_lossy().to_string();
    }

    Ok(Skill {
        name,
        description: description.trim().to_string(),
        instructions: instructions.trim().to_string(),
    })
}

fn seed_default_skills(dir: &Path) -> Result<()> {
    let grep_skill = r#"# grep_search
> Securely and effectively search across vast files and repositories using RipGrep.

Ripgrep (`rg`) is highly optimized. When exploring a repository, always use `rg` FIRST instead of guessing file contents.
Usage example:
```bash
rg -n "struct Agent" src/
```
Never use `find` combined with `grep` if `rg` is installed. It is much faster and respects `.gitignore`.
"#;
    let web_skill = r#"# fetch_webpage
> Extract markdown text representation from public URLs directly from the terminal without breaking due to JavaScript.

When you need to read documentation from a URL, DO NOT use `curl` as it pulls unreadable raw HTML. Instead use:
```bash
npx @postlight/parser "https://example.com/docs"
```
This requires `npx` to be installed and will download a clean markdown string output of the given URL.
"#;
    let jq_skill = r#"# parse_json
> Read specific deeply nested fields from JSON files using `jq` without overloading the terminal.

When analyzing massive `package.json` or configuration files, DO NOT run `cat`. The output will be incredibly large.
Use `jq` to read keys selectively:
```bash
cat package.json | jq '.scripts'
```
"#;

    fs::write(dir.join("grep_search.md"), grep_skill)?;
    fs::write(dir.join("fetch_webpage.md"), web_skill)?;
    fs::write(dir.join("parse_json.md"), jq_skill)?;
    Ok(())
}
