use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "corvus", version, about = "A lightweight local Ollama agent CLI")]
pub struct Args {
    /// Run a single prompt non-interactively and exit
    #[arg(short = 'p', long)]
    pub prompt: Option<String>,

    /// Model name (default: first installed model)
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Ollama base URL
    #[arg(long, default_value = "http://127.0.0.1:11434")]
    pub url: String,

    /// Skip [y/N] confirmation before executing code (DANGEROUS)
    #[arg(long)]
    pub no_confirm: bool,

    /// Per-command execution timeout in seconds
    #[arg(long, default_value_t = 30)]
    pub timeout: u64,

    /// Override built-in system prompt with file contents
    #[arg(long)]
    pub system: Option<PathBuf>,
}
