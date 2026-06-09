//! Smoke-test REPL: spawns `codex app-server` and pipes stdin lines through
//! the AgentBackend trait. Prints each AgentUpdate as it arrives.
//!
//! Usage: `cargo run -p agentline-agent-codex --example codex_repl`
//!
//! Requires `codex` on PATH and prior `codex login`.

use agentline_agent_codex::{CodexBackend, CodexConfig};
use agentline_bridge::{AgentBackend, AgentUpdate};
use futures::StreamExt;
use std::io::{BufRead, Write};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    eprintln!("⏳ spawning `codex app-server`…");
    let backend = CodexBackend::spawn(CodexConfig::default()).await?;

    let cwd = std::env::current_dir()?;
    eprintln!("📁 cwd = {}", cwd.display());

    let sid = backend.new_session(&cwd).await?;
    eprintln!("🆕 session = {sid}");
    eprintln!("💬 type a prompt and hit Enter. /quit to exit.\n");

    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    loop {
        eprint!("> ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        if handle.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/quit" {
            break;
        }

        let mut stream = backend.prompt(&sid, line).await?;
        while let Some(u) = stream.next().await {
            match u {
                AgentUpdate::AssistantText { delta, .. } => {
                    print!("{delta}");
                    std::io::stdout().flush().ok();
                }
                AgentUpdate::ToolCallStart { kind, label, .. } => {
                    eprintln!("\n[{kind:?}] {label}");
                }
                AgentUpdate::ToolCallProgress { output_chunk, .. } => {
                    eprintln!("  | {}", output_chunk.lines().next().unwrap_or(""));
                }
                AgentUpdate::ToolCallEnd { ok, summary, .. } => {
                    eprintln!(
                        "[done={ok}{}]",
                        summary.map(|s| format!(" {s}")).unwrap_or_default()
                    );
                }
                AgentUpdate::Plan { steps } => {
                    eprintln!("\n📋 Plan:");
                    for (i, s) in steps.iter().enumerate() {
                        eprintln!("  {}. {}", i + 1, s);
                    }
                }
                AgentUpdate::PermissionRequest { what, .. } => {
                    eprintln!("\n⚠️  permission: {what}");
                }
                AgentUpdate::Error(msg) => {
                    eprintln!("\n❌ error: {msg}");
                }
                AgentUpdate::Thinking { .. } => {}
                AgentUpdate::ModeChanged { mode_id } => eprintln!("[mode] {mode_id}"),
                AgentUpdate::SessionInfo { title } => eprintln!("[session] {title}"),
                AgentUpdate::ElicitInput { prompt, .. } => eprintln!("[elicit] {prompt}"),
                AgentUpdate::Done => {
                    println!();
                }
            }
        }
    }

    backend.close_session(sid).await?;
    Ok(())
}
