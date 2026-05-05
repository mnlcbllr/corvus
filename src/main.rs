mod agent;
mod cli;
mod executor;
mod ollama;
mod prompt;
mod terminal;
mod tools;
mod tui;
mod skills;

use clap::Parser;
use cli::Args;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Ctrl+C exits cleanly (drops the reqwest connection, signalling Ollama to stop generating)
    tokio::spawn(async {
        tokio::signal::ctrl_c().await.ok();
        std::process::exit(0);
    });

    let args = Args::parse();
    let mut ag = agent::Agent::new(&args).await?;

    if let Some(prompt) = args.prompt.clone() {
        terminal::print_header(&ag.model);
        ag.turn(prompt).await?;
    } else {
        tui::run(ag).await?;
    }

    Ok(())
}
