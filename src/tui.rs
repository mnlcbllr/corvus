use std::{io, sync::Arc};
use anyhow::Result;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt as _;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap, Clear},
    Frame, Terminal,
};
use tokio::sync::{mpsc, Mutex};

use crate::agent::{Agent, AgentMsg};

// ---------------------------------------------------------------------------
// Slash commands registry
// ---------------------------------------------------------------------------

static SLASH_COMMANDS: &[(&str, &str)] = &[
    ("model", "switch or view current model"),
    ("exit",  "quit corvus"),
];

// ---------------------------------------------------------------------------
// Palette state
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Palette {
    Hidden,
    Commands,
    Models(Vec<String>),
}

// ---------------------------------------------------------------------------
// Conversation items
// ---------------------------------------------------------------------------

#[allow(dead_code)]
const VW_BG: Color = Color::Rgb(13, 14, 20);            // #0D0E14
#[allow(dead_code)]
const VW_PANEL: Color = Color::Rgb(21, 23, 34);         // #151722
#[allow(dead_code)]
const VW_HOVER: Color = Color::Rgb(26, 29, 46);         // #1A1D2E
#[allow(dead_code)]
const VW_BORDER: Color = Color::Rgb(30, 34, 53);        // #1E2235
#[allow(dead_code)]
const VW_KEYWORD: Color = Color::Rgb(149, 128, 200);     // #9580C8
#[allow(dead_code)]
const VW_FUNC: Color = Color::Rgb(123, 168, 212);        // #7BA8D4
#[allow(dead_code)]
const VW_STR: Color = Color::Rgb(126, 201, 168);         // #7EC9A8
#[allow(dead_code)]
const VW_CONST: Color = Color::Rgb(212, 165, 240);       // #D4A5F0
#[allow(dead_code)]
const VW_TYPE: Color = Color::Rgb(93, 202, 165);         // #5DCAA5
#[allow(dead_code)]
const VW_COMMENT: Color = Color::Rgb(58, 74, 106);       // #3A4A6A
#[allow(dead_code)]
const VW_TEXT: Color = Color::Rgb(200, 192, 224);        // #C8C0E0
#[allow(dead_code)]
const VW_MUTED: Color = Color::Rgb(106, 122, 154);       // #6A7A9A
#[allow(dead_code)]
const VW_ERR: Color = Color::Rgb(224, 112, 112);         // #E07070

#[derive(Clone)]
enum Item {
    UserMsg(String),
    AssistantText(String),
    CodeFence { lang: String, code: String },
    ConfirmPrompt { lang: String, confirmed: Option<bool> },
    ExecOutput { stdout: String, stderr: String, exit_code: i32 },
    Note(String),
    Splash(String),
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct App {
    items: Vec<Item>,
    input: String,
    cursor: usize,
    scroll: usize,
    auto_scroll: bool,
    model: String,
    thinking: bool,
    tick_count: usize,
    confirming: bool,
    confirm_tx: Option<tokio::sync::oneshot::Sender<bool>>,
    turn_task: Option<tokio::task::JoinHandle<()>>,
    show_thoughts: bool,
    palette: Palette,
    palette_sel: usize,
}

impl App {
    fn push_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn delete_back(&mut self) {
        if self.cursor > 0 {
            let ch = self.input[..self.cursor].chars().last().unwrap();
            self.cursor -= ch.len_utf8();
            self.input.remove(self.cursor);
        }
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            let ch = self.input[..self.cursor].chars().last().unwrap();
            self.cursor -= ch.len_utf8();
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.input.len() {
            let ch = self.input[self.cursor..].chars().next().unwrap();
            self.cursor += ch.len_utf8();
        }
    }

    fn append_chunk(&mut self, chunk: &str) {
        match self.items.last_mut() {
            Some(Item::AssistantText(ref mut t)) => t.push_str(chunk),
            _ => self.items.push(Item::AssistantText(chunk.to_string())),
        }
    }

    fn update_confirm_prompt(&mut self, accepted: Option<bool>) {
        for item in self.items.iter_mut().rev() {
            if let Item::ConfirmPrompt { confirmed, .. } = item {
                *confirmed = accepted;
                return;
            }
        }
    }

    fn palette_visible_items(&self) -> Vec<(String, String)> {
        match &self.palette {
            Palette::Hidden => vec![],
            Palette::Commands => {
                let filter = self.input.strip_prefix('/').unwrap_or("").to_lowercase();
                SLASH_COMMANDS.iter()
                    .filter(|(cmd, _)| cmd.starts_with(filter.as_str()))
                    .map(|(cmd, desc)| (format!("/{}", cmd), desc.to_string()))
                    .collect()
            }
            Palette::Models(list) => {
                let filter = self.input.to_lowercase();
                list.iter()
                    .filter(|m| m.to_lowercase().contains(filter.as_str()))
                    .map(|m| (m.clone(), String::new()))
                    .collect()
            }
        }
    }

    fn clamped_sel(&self, items_len: usize) -> usize {
        if items_len == 0 { 0 } else { self.palette_sel.min(items_len - 1) }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().fg(VW_TEXT)), area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(area);

    // Status bar
    let hint = match &app.palette {
        Palette::Commands => "  [↑↓] select  [enter] choose  [tab] complete  [esc] close".to_string(),
        Palette::Models(_) => "  [↑↓] select model  [enter] switch  [esc] back".to_string(),
        Palette::Hidden => if app.thinking {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let frame = frames[app.tick_count % frames.len()];
            format!("  {} thinking…", frame)
        } else if app.confirming {
            "  [y] run  [n] decline".to_string()
        } else {
            "  [/] cmds  [ctrl+t] thoughts  [↑↓] scroll  [ctrl+c] quit".to_string()
        },
    };
    
    let status_line = format!(" corvus — {}{}", app.model, hint);
    let status = Paragraph::new(status_line)
        .style(Style::default().fg(VW_TEXT).add_modifier(Modifier::BOLD));
    f.render_widget(status, chunks[0]);

    // History
    let history_area = chunks[1].inner(Margin { vertical: 0, horizontal: 2 });
    let history_width = history_area.width as usize;
    let lines = build_lines(&app.items, history_width, app.show_thoughts);
    let total = lines.len();
    let visible = history_area.height as usize;
    let scroll_y = if app.auto_scroll {
        total.saturating_sub(visible) as u16
    } else {
        app.scroll as u16
    };
    let history = Paragraph::new(lines)
        .style(Style::default().fg(VW_TEXT))
        .scroll((scroll_y, 0))
        .wrap(Wrap { trim: false });
    f.render_widget(history, history_area);

    // Input box
    let input_area = chunks[2].inner(Margin { vertical: 0, horizontal: 2 });
    let palette_open = !matches!(app.palette, Palette::Hidden);
    let (border_style, title) = if app.confirming {
        (Style::default().fg(VW_FUNC), " [y] yes  [n / esc] no ")
    } else if palette_open {
        (Style::default().fg(VW_KEYWORD), " command ")
    } else {
        (Style::default().fg(VW_BORDER), " message ")
    };

    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = input_block.inner(input_area);
    f.render_widget(input_block, input_area);

    let display = if app.confirming {
        Paragraph::new(Line::from(vec![
            Span::styled("run block? ", Style::default().fg(VW_FUNC)),
            Span::styled("[y] yes   [n / esc] no", Style::default().fg(VW_MUTED)),
        ]))
    } else {
        Paragraph::new(app.input.as_str())
            .style(Style::default().fg(VW_TEXT))
    };
    f.render_widget(display, inner);

    // Cursor
    if !app.confirming {
        let cx = inner.x + app.input[..app.cursor].chars().count() as u16;
        let cy = inner.y;
        if cx < inner.x + inner.width && cy < inner.y + inner.height {
            f.set_cursor_position((cx, cy));
        }
    }

    // Palette overlay (rendered last so it's on top)
    if palette_open {
        render_palette(f, app, input_area);
    }
}

fn render_palette(f: &mut Frame, app: &App, input_rect: Rect) {
    let items = app.palette_visible_items();
    if items.is_empty() {
        return;
    }

    let sel = app.clamped_sel(items.len());
    let max_h = 10u16;
    let h = (items.len() as u16 + 2).min(max_h);
    let w = input_rect.width.min(62);
    let y = input_rect.y.saturating_sub(h);
    let popup = Rect::new(input_rect.x, y, w, h);

    let title = match &app.palette {
        Palette::Commands => " commands ",
        Palette::Models(_) => " select model ",
        Palette::Hidden => "",
    };

    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(VW_KEYWORD))
        .title(Span::styled(title, Style::default().fg(VW_KEYWORD).add_modifier(Modifier::BOLD)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let visible_h = inner.height as usize;
    let start = sel.saturating_sub(visible_h.saturating_sub(1));

    for (i, (cmd, desc)) in items.iter().enumerate().skip(start).take(visible_h) {
        let row = Rect::new(inner.x, inner.y + (i - start) as u16, inner.width, 1);
        let is_sel = i == sel;

        let line = if desc.is_empty() {
            // Model picker row
            Line::from(Span::styled(
                format!(" {}", cmd),
                if is_sel {
                    Style::default().fg(VW_CONST).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(VW_TEXT)
                },
            ))
        } else {
            // Command row with description
            let cmd_col = format!(" {:14}", cmd);
            if is_sel {
                Line::from(vec![
                    Span::styled(cmd_col, Style::default().fg(VW_CONST).add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" {}", desc), Style::default().fg(VW_COMMENT)),
                ])
            } else {
                Line::from(vec![
                    Span::styled(cmd_col, Style::default().fg(VW_KEYWORD)),
                    Span::styled(format!(" {}", desc), Style::default().fg(VW_COMMENT)),
                ])
            }
        };

        f.render_widget(Paragraph::new(line), row);
    }
}

fn build_lines(items: &[Item], width: usize, show_thoughts: bool) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let y = Style::default().fg(VW_FUNC);
    let g = Style::default().fg(VW_COMMENT);
    let gr = Style::default().fg(VW_MUTED);

    for item in items {
        match item {
            Item::UserMsg(text) => {
                out.push(Line::from(vec![
                    Span::styled("You ❯ ".to_string(), Style::default().fg(VW_KEYWORD).add_modifier(Modifier::BOLD)),
                    Span::raw(text.clone()),
                ]));
                out.push(Line::from(""));
            }
            Item::AssistantText(text) => {
                let mut current_idx = 0;
                while current_idx < text.len() {
                    let (segment, is_thought, end_idx) = if let Some(think_start) = text[current_idx..].find("<think>") {
                        let actual_start = current_idx + think_start;
                        if actual_start > current_idx {
                            (&text[current_idx..actual_start], false, actual_start)
                        } else {
                            if let Some(think_end) = text[actual_start..].find("</think>") {
                                (&text[actual_start + 7..actual_start + think_end], true, actual_start + think_end + 8)
                            } else {
                                (&text[actual_start + 7..], true, text.len())
                            }
                        }
                    } else {
                        (&text[current_idx..], false, text.len())
                    };

                    if is_thought {
                        if !show_thoughts {
                            out.push(Line::from(Span::styled("  💭 [thought process hidden]", Style::default().fg(VW_COMMENT).add_modifier(Modifier::DIM))));
                        } else {
                            if out.is_empty() || !out.last().unwrap().spans.is_empty() {
                                out.push(Line::from(""));
                            }
                            for line in segment.lines() {
                                out.push(Line::from(vec![
                                    Span::styled("  │ ", Style::default().fg(VW_COMMENT)),
                                    Span::styled(line.to_string(), Style::default().fg(VW_COMMENT).add_modifier(Modifier::DIM)),
                                ]));
                            }
                            out.push(Line::from(""));
                        }
                    } else {
                        // Sometimes trailing/leading newlines from tags can create excess gaps, handle carefully
                        let mut seg_lines = segment.lines().peekable();
                        while let Some(line) = seg_lines.next() {
                            if line.is_empty() && is_thought { continue; }
                            out.push(Line::from(Span::raw(line.to_string())));
                        }
                    }

                    current_idx = end_idx;
                    // Prevent infinite loops if parsing fails
                    if current_idx >= text.len() || end_idx == 0 { break; }
                }
            }
            Item::CodeFence { lang, code } => {
                out.push(Line::from(Span::styled(format!("  ┌─ {} ─", lang), y)));
                for line in code.lines() {
                    out.push(Line::from(vec![
                        Span::styled("  │ ".to_string(), y),
                        Span::raw(line.to_string()),
                    ]));
                }
                out.push(Line::from(Span::styled("  └────────".to_string(), y)));
                out.push(Line::from(""));
            }
            Item::ConfirmPrompt { lang, confirmed } => {
                let (sym, color, label) = match confirmed {
                    Some(true)  => ("✓", VW_STR, "executed"),
                    Some(false) => ("✗", VW_ERR,   "declined"),
                    None        => ("?", VW_FUNC, "awaiting confirmation…"),
                };
                out.push(Line::from(vec![
                    Span::styled(format!("  {} ", sym), Style::default().fg(color)),
                    Span::styled(format!("{} block — ", lang), g),
                    Span::styled(label.to_string(), Style::default().fg(color)),
                ]));
            }
            Item::ExecOutput { stdout, stderr, exit_code } => {
                for line in stdout.lines() {
                    out.push(Line::from(Span::styled(format!("  {}", line), gr)));
                }
                for line in stderr.lines() {
                    out.push(Line::from(Span::styled(
                        format!("  {}", line),
                        Style::default().fg(VW_ERR),
                    )));
                }
                let ec_style = if *exit_code == 0 { Style::default().fg(VW_STR) } else { Style::default().fg(VW_ERR) };
                out.push(Line::from(Span::styled(format!("  [exit {}]", exit_code), ec_style)));
                out.push(Line::from(""));
            }
            Item::Note(text) => {
                for line in text.lines() {
                    out.push(Line::from(Span::styled(format!("  {}", line), g)));
                }
                out.push(Line::from(""));
            }
            Item::Splash(ref model) => {
                out.extend(corvus_splash_styled(model, width));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Event handling
// ---------------------------------------------------------------------------

fn handle_agent_msg(msg: AgentMsg, app: &mut App) {
    match msg {
        AgentMsg::Chunk(text) => {
            app.thinking = false;
            app.append_chunk(&text);
        }
        AgentMsg::BlockAutoRan(block) => {
            app.items.push(Item::CodeFence { lang: block.lang.clone(), code: block.code });
            app.items.push(Item::ConfirmPrompt { lang: block.lang, confirmed: Some(true) });
        }
        AgentMsg::BlockProposed { block, confirm_tx } => {
            app.items.push(Item::CodeFence { lang: block.lang.clone(), code: block.code });
            app.items.push(Item::ConfirmPrompt { lang: block.lang, confirmed: None });
            app.confirming = true;
            app.confirm_tx = Some(confirm_tx);
        }
        AgentMsg::BlockResult { result, .. } => {
            app.update_confirm_prompt(Some(true));
            app.confirming = false;
            app.items.push(Item::ExecOutput {
                stdout: result.stdout,
                stderr: result.stderr,
                exit_code: result.exit_code,
            });
        }
        AgentMsg::TurnComplete => {
            app.thinking = false;
            app.confirming = false;
        }
        AgentMsg::Error(e) => {
            app.thinking = false;
            app.confirming = false;
            app.items.push(Item::ExecOutput {
                stdout: String::new(),
                stderr: e,
                exit_code: 1,
            });
        }
    }
}

async fn execute_palette_selection(
    app: &mut App,
    agent: &Arc<Mutex<Agent>>,
    _tx: &mpsc::UnboundedSender<AgentMsg>,
) -> Result<bool> {
    match app.palette.clone() {
        Palette::Commands => {
            let filter = app.input.strip_prefix('/').unwrap_or("").to_lowercase();
            let visible: Vec<(&str, &str)> = SLASH_COMMANDS.iter()
                .filter(|(cmd, _)| cmd.starts_with(filter.as_str()))
                .map(|(c, d)| (*c, *d))
                .collect();
            let sel = app.clamped_sel(visible.len());
            let chosen = visible.get(sel).map(|(c, _)| *c);

            app.palette = Palette::Hidden;
            app.input.clear();
            app.cursor = 0;
            app.palette_sel = 0;

            match chosen {
                Some("exit") => return Ok(true),
                Some("model") => {
                    let models = agent.lock().await.list_models().await.unwrap_or_default();
                    app.palette = Palette::Models(models);
                    app.palette_sel = 0;
                }
                _ => {}
            }
        }
        Palette::Models(list) => {
            let filter = app.input.to_lowercase();
            let visible: Vec<&String> = list.iter()
                .filter(|m| m.to_lowercase().contains(filter.as_str()))
                .collect();
            let sel = app.clamped_sel(visible.len());
            if let Some(model) = visible.get(sel) {
                let model = (*model).clone();
                agent.lock().await.model = model.clone();
                app.model = model.clone();
                app.items.push(Item::Note(format!("Switched to: {}", model)));
            }
            app.palette = Palette::Hidden;
            app.input.clear();
            app.cursor = 0;
            app.palette_sel = 0;
        }
        Palette::Hidden => {}
    }
    Ok(false)
}

fn tab_complete_palette(app: &mut App) {
    let items = app.palette_visible_items();
    let sel = app.clamped_sel(items.len());
    if let Some((cmd, _)) = items.get(sel) {
        match &app.palette {
            Palette::Commands => {
                app.input = cmd.clone();
                app.cursor = app.input.len();
            }
            Palette::Models(_) => {
                app.input = cmd.clone();
                app.cursor = app.input.len();
            }
            Palette::Hidden => {}
        }
        app.palette_sel = 0;
    }
}

async fn handle_key(
    key: KeyEvent,
    app: &mut App,
    agent: &Arc<Mutex<Agent>>,
    tx: &mpsc::UnboundedSender<AgentMsg>,
) -> Result<bool> {
    // Ctrl+C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(true);
    }

    // Confirmation mode
    if app.confirming {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(t) = app.confirm_tx.take() { let _ = t.send(true); }
                app.update_confirm_prompt(Some(true));
                app.confirming = false;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                if let Some(t) = app.confirm_tx.take() { let _ = t.send(false); }
                app.update_confirm_prompt(Some(false));
                app.confirming = false;
            }
            _ => {}
        }
        return Ok(false);
    }

    // Cancel generation
    if app.thinking && key.code == KeyCode::Esc {
        if let Some(task) = app.turn_task.take() {
            task.abort();
            app.thinking = false;
            app.items.push(Item::Note("Generation cancelled.".to_string()));
        }
        return Ok(false);
    }

    // Palette mode — intercept keys before normal handling
    if !matches!(app.palette, Palette::Hidden) {
        match key.code {
            KeyCode::Esc => {
                app.palette = Palette::Hidden;
                app.input.clear();
                app.cursor = 0;
                app.palette_sel = 0;
            }
            KeyCode::Up => {
                app.palette_sel = app.palette_sel.saturating_sub(1);
            }
            KeyCode::Down => {
                let max = app.palette_visible_items().len().saturating_sub(1);
                app.palette_sel = (app.palette_sel + 1).min(max);
            }
            KeyCode::Tab => {
                tab_complete_palette(app);
            }
            KeyCode::Enter => {
                return execute_palette_selection(app, agent, tx).await;
            }
            KeyCode::Backspace => {
                app.delete_back();
                app.palette_sel = 0;
                // Close Commands palette if user deletes the leading /
                if matches!(app.palette, Palette::Commands) && !app.input.starts_with('/') {
                    app.palette = Palette::Hidden;
                }
            }
            KeyCode::Char(c) => {
                app.push_char(c);
                app.palette_sel = 0;
            }
            _ => {}
        }
        return Ok(false);
    }

    // Normal input mode
    match (key.modifiers, key.code) {
        (_, KeyCode::Enter) => {
            if app.thinking { return Ok(false); }
            let input = std::mem::take(&mut app.input);
            app.cursor = 0;
            if input.trim().is_empty() { return Ok(false); }

            // Normal turn
            app.items.push(Item::UserMsg(input.clone()));
            app.thinking = true;
            app.auto_scroll = true;

            let tx2 = tx.clone();
            let ag = Arc::clone(agent);
            let handle = tokio::spawn(async move {
                let mut ag = ag.lock().await;
                if let Err(e) = ag.turn_tui(input, tx2.clone()).await {
                    let _ = tx2.send(AgentMsg::Error(e.to_string()));
                }
            });
            app.turn_task = Some(handle);
        }

        // Open palette on / when input is empty
        (m, KeyCode::Char('/')) if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) => {
            if app.input.is_empty() {
                app.palette = Palette::Commands;
                app.palette_sel = 0;
            }
            app.push_char('/');
        }

        (_, KeyCode::Backspace) => { app.delete_back(); }
        (_, KeyCode::Left)      => { app.move_left(); }
        (_, KeyCode::Right)     => { app.move_right(); }
        (_, KeyCode::Home)      => { app.cursor = 0; }
        (_, KeyCode::End)       => { app.cursor = app.input.len(); }

        (KeyModifiers::CONTROL, KeyCode::Char('a')) => { app.cursor = 0; }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => { app.cursor = app.input.len(); }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => { app.input.clear(); app.cursor = 0; }
        (KeyModifiers::CONTROL, KeyCode::Char('k')) => { app.input.truncate(app.cursor); }
        (KeyModifiers::CONTROL, KeyCode::Char('l')) => { app.auto_scroll = true; app.scroll = 0; }
        (KeyModifiers::CONTROL, KeyCode::Char('t')) => { app.show_thoughts = !app.show_thoughts; app.auto_scroll = true; }

        (_, KeyCode::Up)       => { app.auto_scroll = false; app.scroll = app.scroll.saturating_sub(1); }
        (_, KeyCode::Down)     => { app.scroll += 1; }
        (_, KeyCode::PageUp)   => { app.auto_scroll = false; app.scroll = app.scroll.saturating_sub(10); }
        (_, KeyCode::PageDown) => { app.scroll += 10; }

        (m, KeyCode::Char(c)) if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) => {
            app.push_char(c);
        }
        _ => {}
    }
    Ok(false)
}

fn get_git_config(key: &str) -> Option<String> {
    std::process::Command::new("git")
        .args(&["config", "--get", key])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn get_user_first_name() -> String {
    get_git_config("user.name")
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "User".to_string())
        .split_whitespace()
        .next()
        .unwrap_or("User")
        .to_string()
}

fn get_user_email() -> Option<String> {
    get_git_config("user.email")
}

fn get_display_cwd() -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let cwd_str = cwd.to_string_lossy().to_string();
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy().to_string();
        if cwd_str.starts_with(&home_str) {
            return cwd_str.replacen(&home_str, "~", 1);
        }
    }
    cwd_str
}

fn corvus_splash_styled(model: &str, _width: usize) -> Vec<Line<'static>> {
    let version = env!("CARGO_PKG_VERSION");
    
    let w1: usize = 45;
    let w2: usize = 68;
    
    let top_dash = _width.saturating_sub(15 + version.len());
    let top = format!("╭─── Corvus v{} {}╮", version, "─".repeat(top_dash));
    let bottom = format!("╰{}╯", "─".repeat(_width.saturating_sub(2)));
    let w3 = _width.saturating_sub(w1 + w2 + 3);

    let pad_cw_span = |s: &str, w: usize, style: Style| -> Span<'static> {
        let chars_count = s.chars().count();
        if chars_count >= w {
            let truncated: String = s.chars().take(w).collect();
            return Span::styled(truncated, style);
        }
        let space = w - chars_count;
        let left = space / 2;
        let right = space - left;
        Span::styled(format!("{}{}{}", "\u{00A0}".repeat(left), s, "\u{00A0}".repeat(right)), style)
    };

    let pad_lw_span = |s: &str, w2: usize, w3: usize, style: Style| -> Vec<Span<'static>> {
        let chars_count = s.chars().count();
        let mut spans = Vec::new();
        if chars_count >= w2 {
            let truncated: String = s.chars().take(w2).collect();
            spans.push(Span::styled(truncated, style));
        } else {
            spans.push(Span::styled(format!("{}{}", s, "\u{00A0}".repeat(w2 - chars_count)), style));
        }
        spans.push(Span::raw("\u{00A0}".repeat(w3)));
        spans
    };

    let pad_sep = |total: usize, style: Style| -> Vec<Span<'static>> {
        let dashes = total.saturating_sub(2);
        vec![Span::styled(format!(" {} ", "─".repeat(dashes)), style)]
    };

    let welcome_msg = format!("Welcome back {}!", get_user_first_name());
    let org_msg = if let Some(email) = get_user_email() {
        format!("{}'s Organization", email)
    } else {
        "Local Organization".to_string()
    };
    let dir_msg = get_display_cwd();
    let pro_msg = format!("{} · Corvus ", model);

    let h1_style = Style::default().fg(VW_KEYWORD).add_modifier(Modifier::BOLD);
    let art_style = Style::default().fg(VW_KEYWORD);
    let txt_style = Style::default().fg(VW_TEXT);
    let mut_style = Style::default().fg(VW_MUTED);
    let brd_style = Style::default().fg(VW_KEYWORD);

    let rows = vec![
        (pad_cw_span("", w1, txt_style), pad_lw_span(" Tips for getting started", w2, w3, h1_style)),
        (pad_cw_span(&welcome_msg, w1, h1_style), pad_lw_span(" Run /init to create a CORVUS.md file with instructions for Corvus", w2, w3, txt_style)),
        (pad_cw_span("     ▄▄▄▄       ", w1, art_style), pad_sep(w2 + w3, mut_style)),
        (pad_cw_span("    ◄█▄████     ", w1, art_style), pad_lw_span(" Tips for getting started", w2, w3, h1_style)), 
        (pad_cw_span("     ██████     ", w1, art_style), pad_lw_span(" Use `/model` to change and select models", w2, w3, txt_style)),
        (pad_cw_span("      ▘  ▘      ", w1, art_style), pad_lw_span(" Use `/exit` to quit Corvus", w2, w3, txt_style)),
        (pad_cw_span(&pro_msg, w1, mut_style), pad_lw_span("", w2, w3, txt_style)),
        (pad_cw_span(&org_msg, w1, mut_style), pad_lw_span("", w2, w3, txt_style)),
        (pad_cw_span(&dir_msg, w1, mut_style), pad_lw_span("", w2, w3, txt_style)),
    ];

  
    
    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(Line::from(Span::styled(top, brd_style)));
    for (l_span, r_spans) in rows {
        let mut row = Vec::new();
        row.push(Span::styled("│", brd_style));
        row.push(l_span);
        row.push(Span::styled("│", brd_style));
        row.extend(r_spans);
        row.push(Span::styled("│", brd_style));
        out.push(Line::from(row));
    }
    out.push(Line::from(Span::styled(bottom, brd_style)));
    out
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run(agent: Agent) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen);
        original_hook(info);
    }));

    let model = agent.model.clone();
    let agent = Arc::new(Mutex::new(agent));
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentMsg>();

    let mut app = App {
        items: vec![Item::Splash(model.clone())],
        input: String::new(),
        cursor: 0,
        scroll: 0,
        auto_scroll: true,
        model,
        thinking: false,
        tick_count: 0,
        confirming: false,
        confirm_tx: None,
        turn_task: None,
        show_thoughts: true,
        palette: Palette::Hidden,
        palette_sel: 0,
    };

    let mut events = EventStream::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(80));

    loop {
        terminal.draw(|f| render(f, &app))?;

        tokio::select! {
            _ = interval.tick() => {
                if app.thinking {
                    app.tick_count = app.tick_count.wrapping_add(1);
                }
            }
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        if handle_key(key, &mut app, &agent, &tx).await? {
                            break;
                        }
                    }
                    Some(Ok(Event::Resize(..))) => {}
                    None | Some(Err(_)) => break,
                    _ => {}
                }
            }
            maybe_msg = rx.recv() => {
                if let Some(msg) = maybe_msg {
                    handle_agent_msg(msg, &mut app);
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
