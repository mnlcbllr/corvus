use crossterm::{
    execute,
    style::{Color, Print, ResetColor, SetForegroundColor},
};
use std::io::{self, Write as _, stdout};

use crate::executor::ExecResult;

#[allow(dead_code)]
const VW_KEYWORD: Color = Color::Rgb { r: 149, g: 128, b: 200 };
#[allow(dead_code)]
const VW_FUNC: Color = Color::Rgb { r: 123, g: 168, b: 212 };
#[allow(dead_code)]
const VW_STR: Color = Color::Rgb { r: 126, g: 201, b: 168 };
#[allow(dead_code)]
const VW_COMMENT: Color = Color::Rgb { r: 58, g: 74, b: 106 };
#[allow(dead_code)]
const VW_MUTED: Color = Color::Rgb { r: 106, g: 122, b: 154 };
#[allow(dead_code)]
const VW_ERR: Color = Color::Rgb { r: 224, g: 112, b: 112 };

pub fn print_header(model: &str) {
    execute!(
        stdout(),
        SetForegroundColor(VW_COMMENT),
        Print(format!("corvus — using model: {}\n", model)),
        ResetColor,
    )
    .ok();
    stdout().flush().ok();
}

pub fn print_assistant_label() {
    execute!(
        stdout(),
        SetForegroundColor(VW_KEYWORD),
        Print("corvus> "),
        ResetColor,
    )
    .ok();
    stdout().flush().ok();
}

pub fn print_thinking() {
    execute!(
        stdout(),
        SetForegroundColor(VW_COMMENT),
        Print("thinking..."),
        ResetColor,
    ).ok();
    stdout().flush().ok();
}

pub fn clear_thinking() {
    execute!(
        stdout(),
        crossterm::cursor::MoveLeft(11),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
    ).ok();
    stdout().flush().ok();
}

pub fn stream_chunk(s: &str) {
    print!("{}", s);
    stdout().flush().ok();
}

pub fn print_code_fence(lang: &str, code: &str) {
    let label = if lang.is_empty() { "shell" } else { lang };
    execute!(
        stdout(),
        SetForegroundColor(VW_FUNC),
        Print(format!("  ┌─ {} ─\n", label)),
        ResetColor,
    )
    .ok();
    for line in code.lines() {
        execute!(
            stdout(),
            SetForegroundColor(VW_FUNC),
            Print(format!("  │ {}\n", line)),
            ResetColor,
        )
        .ok();
    }
    execute!(
        stdout(),
        SetForegroundColor(VW_FUNC),
        Print("  └────────\n\n"),
        ResetColor,
    )
    .ok();
    stdout().flush().ok();
}

pub fn print_edit_block(code: &str) {
    // Parse and render with red/green diff colors instead of plain yellow
    #[derive(PartialEq)]
    enum Section { Header, Old, New }
    let mut section = Section::Header;

    for line in code.lines() {
        match section {
            Section::Header => {
                if let Some(p) = line.strip_prefix("file:") {
                    let file_label = p.trim();
                    execute!(
                        stdout(),
                        SetForegroundColor(VW_FUNC),
                        Print(format!("  ┌─ edit: {} ─\n", file_label)),
                        ResetColor,
                    ).ok();
                } else if line.trim().starts_with("<<<") {
                    section = Section::Old;
                    execute!(stdout(), SetForegroundColor(VW_COMMENT), Print("  │ ···\n"), ResetColor).ok();
                }
            }
            Section::Old => {
                if line.trim().starts_with("===") || line.trim().starts_with("---") {
                    section = Section::New;
                    execute!(stdout(), SetForegroundColor(VW_COMMENT), Print("  │ ···\n"), ResetColor).ok();
                } else {
                    execute!(
                        stdout(),
                        SetForegroundColor(VW_ERR),
                        Print(format!("  │ - {}\n", line)),
                        ResetColor,
                    ).ok();
                }
            }
            Section::New => {
                if line.trim().starts_with(">>>") {
                    break;
                }
                execute!(
                    stdout(),
                    SetForegroundColor(VW_KEYWORD),
                    Print(format!("  │ + {}\n", line)),
                    ResetColor,
                ).ok();
            }
        }
    }

    execute!(
        stdout(),
        SetForegroundColor(VW_FUNC),
        Print("  └────────\n\n"),
        ResetColor,
    ).ok();
    stdout().flush().ok();
}

pub fn print_exec_output(result: &ExecResult) {
    if !result.stdout.is_empty() {
        execute!(
            stdout(),
            SetForegroundColor(VW_COMMENT),
            Print("  [stdout]\n"),
            ResetColor,
        )
        .ok();
        for line in result.stdout.trim_end().lines() {
            execute!(
                stdout(),
                SetForegroundColor(VW_MUTED),
                Print(format!("  {}\n", line)),
                ResetColor,
            )
            .ok();
        }
    }
    if !result.stderr.is_empty() {
        execute!(
            stdout(),
            SetForegroundColor(VW_ERR),
            Print("  [stderr]\n"),
            ResetColor,
        )
        .ok();
        for line in result.stderr.trim_end().lines() {
            execute!(
                stdout(),
                SetForegroundColor(VW_ERR),
                Print(format!("  {}\n", line)),
                ResetColor,
            )
            .ok();
        }
    }
    let (color, label) = if result.timed_out {
        (VW_ERR, "timed out".to_string())
    } else if result.exit_code == 0 {
        (VW_STR, format!("exit {}", result.exit_code))
    } else {
        (VW_ERR, format!("exit {}", result.exit_code))
    };
    execute!(
        stdout(),
        SetForegroundColor(color),
        Print(format!("  [{}]\n\n", label)),
        ResetColor,
    )
    .ok();
    stdout().flush().ok();
}


pub fn print_warning(msg: &str) {
    execute!(
        stdout(),
        SetForegroundColor(VW_FUNC),
        Print(format!("warning: {}\n", msg)),
        ResetColor,
    )
    .ok();
    stdout().flush().ok();
}

pub fn confirm(prompt: &str) -> bool {
    execute!(
        stdout(),
        SetForegroundColor(VW_FUNC),
        Print(format!("{} [y/N] ", prompt)),
        ResetColor,
    )
    .ok();
    stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}
