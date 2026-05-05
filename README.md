# Corvus

**Corvus** is a highly autonomous, context-aware AI coding assistant designed specifically for the terminal. Built in Rust and powered by local Ollama models (such as `qwen2.5-coder:7b`), Corvus provides an elegant, interactive TUI (Terminal User Interface) modeled after professional tools like Claude Code. It allows developers to collaborate with AI seamlessly, right within their workspace.

![Corvus Splash](https://via.placeholder.com/800x400?text=Corvus+Terminal+TUI)

## 🌟 Features

- **Sleek Ratatui Interface**: A polished, custom-branded terminal UI modeled tightly around Claude Code's aesthetics. Includes interactive command palettes, keyboard navigation, and beautiful syntax highlighting.
- **Void Wing Aesthetic**: A tailored, cool-toned color palette built natively without solid backgrounds—allowing your terminal's natural transparency to shine through while highlighting text with distinct `#9580C8` primary accents.
- **Responsive Layout Architecture**: The dashboard dynamically adapts bounding lines and separating borders directly to your custom terminal footprint while strictly adhering to a crisp dual-column splash layout array.
- **Local AI Execution**: Uses **Ollama** locally, ensuring code privacy with zero external API calls.
- **Autonomous Coding & Command Execution**: Capable of generating, proposing, and selectively executing terminal commands and scripts directly within the TUI.
- **Dynamic Skill Injection**: Systematically manages context limits by dynamically resolving and injecting necessary "skills" only when context calls for them.
- **Zero-Shot Semantic Routing**: Autonomously determines on-the-fly tool selection to streamline context processing without suffering from context bloat or "goal amnesia".
- **Transparent Thought Process**: With `[ctrl+t]`, you can toggle displaying the AI's internal `<think>` process to better understand its reasoning.

## 🛠️ Installation & Setup

Ensure you have [Rust](https://rustup.rs/) and [Ollama](https://ollama.ai/) installed on your machine.

1. **Clone the repository** (if applicable):
   ```bash
   git clone https://github.com/your-username/corvus.git
   cd corvus
   ```

2. **Pull a local LLM via Ollama**:
   ```bash
   ollama pull qwen2.5-coder:7b
   ```

3. **Build the application**:
   ```bash
   cargo build --release
   ```

4. **Run Corvus**:
   ```bash
   cargo run --release -- -m "qwen2.5-coder:7b"
   ```

## ⌨️ TUI Keyboard Shortcuts

| Shortcut | Action                                                 |
|----------|--------------------------------------------------------|
| `Ctrl+C` | Quit Corvus                                            |
| `Ctrl+T` | Toggle displaying the AI's `[thought process]`         |
| `/`      | Bring up the command palette (`/exit`, `/model`, etc.) |
| `[↑↓]`   | Scroll through the conversation history or palette     |
| `[Enter]`| Send message or choose palette selection               |

## 🏗️ Architecture Overview

The codebase is built on modern Rust async ecosystem (`tokio`), HTTP clients (`reqwest`), and interactive rendering (`ratatui` + `crossterm`).

- `src/agent.rs`: Core agentic lifecycle, turn orchestration, and tool selection.
- `src/executor.rs`: Secure handling of script execution constraints and stdout/stderr capturing.
- `src/ollama.rs`: Client logic interfacing with the locally running Ollama daemon.
- `src/skills.rs`: The skill injection layer helping manage token limits intelligently.
- `src/prompt.rs`: System directives preventing AI behavioral drift.
- `src/tui.rs`: The Ratatui implementation of the event loop and user interface.

## 🤝 Contributing

Contributions, issues, and feature requests are welcome! Feel free to check the [issues page](#). 

## 📝 License

This project is licensed under the MIT License.
