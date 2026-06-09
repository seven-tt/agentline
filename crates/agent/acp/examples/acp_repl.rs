//! Generic ACP REPL: drives any ACP-speaking agent given a command line.
//!
//! Usage:
//!   cargo run -p agentline-agent-acp --example acp_repl -- <command> [args...]
//!
//! For Claude Code specifically, prefer the `claude_code_repl` example in
//! the `agentline-agent-claude-code` crate — it sets up the env scrubbing
//! and ~/.claude/settings.json plumbing.

use agentline_agent_acp::{AcpBackend, AcpBackendConfig};
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

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: acp_repl <command> [args...]");
        std::process::exit(2);
    }
    let command = args.remove(0);

    eprintln!("⏳ spawning ACP agent: {command} {args:?}");
    let backend = AcpBackend::spawn(AcpBackendConfig::new(command, args)).await?;

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
                AgentUpdate::ToolCallEnd { ok, .. } => {
                    eprintln!("[done={ok}]");
                }
                AgentUpdate::PermissionRequest { id, what, .. } => {
                    eprintln!("\n⚠️  permission: {what} → auto-allow (id={id})");
                    backend.answer_permission(&sid, &id, true).await?;
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
                _ => {}
            }
        }
    }

    backend.close_session(sid).await?;
    Ok(())
}
