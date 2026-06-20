//! Drives the *real* claude-code agent over ACP and makes it run a shell
//! command via its Bash tool, so we can see the environment that command
//! actually inherits — in particular whether our injected `NO_PROXY` (with the
//! LAN ranges) reaches the agent's shell.
//!
//! Ignored by default: needs network + a working Claude auth (OAuth keychain or
//! a proxy/base-url). Run explicitly:
//!
//! ```sh
//! cargo test -p agentline-agent-claude-code --test agent_env -- --ignored --nocapture
//! ```

use agentline_agent_claude_code::{ClaudeCodeConfig, spawn};
use agentline_bridge::{AgentBackend, AgentUpdate, types::text_prompt};
use futures::StreamExt;
use std::time::Duration;

#[tokio::test]
#[ignore = "drives the real claude-code agent; needs network + auth"]
async fn agent_bash_inherits_no_proxy() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("agentline=debug,info")
        .try_init();

    let backend = spawn(ClaudeCodeConfig::default())
        .await
        .expect("spawn claude-code agent");

    let cwd = std::env::temp_dir();
    let sid = backend.new_session(&cwd).await.expect("new session");

    let outfile = std::env::temp_dir().join("agentline_agent_env_dump.txt");
    let _ = std::fs::remove_file(&outfile);

    // Be very explicit so the model just runs the one command.
    let prompt = format!(
        "Use the Bash tool to run exactly this single command and then stop. Do not explain. \
         Command: env | sort > {}",
        outfile.display()
    );

    let run = async {
        let content = text_prompt(prompt);
        let mut stream = backend.prompt(&sid, &content).await.expect("prompt");
        while let Some(update) = stream.next().await {
            match update {
                AgentUpdate::PermissionRequest { id, what, .. } => {
                    eprintln!("[test] approving permission: {what}");
                    let _ = backend.respond_permission(&sid, &id, true).await;
                }
                AgentUpdate::Session(agentline_bridge::SessionUpdate::ToolCall(tc)) => {
                    eprintln!("[test] tool start: {}", tc.title);
                }
                AgentUpdate::Error(e) => {
                    eprintln!("[test] agent error: {e}");
                    break;
                }
                AgentUpdate::Done => break,
                _ => {}
            }
        }
    };

    tokio::time::timeout(Duration::from_secs(120), run)
        .await
        .expect("agent run timed out");

    let dump = std::fs::read_to_string(&outfile)
        .expect("agent did not write the env dump — it may not have run the command");

    let proxy_env: Vec<&str> = dump
        .lines()
        .filter(|l| {
            let u = l.to_uppercase();
            u.starts_with("NO_PROXY=")
                || u.starts_with("HTTP_PROXY=")
                || u.starts_with("HTTPS_PROXY=")
                || u.starts_with("ALL_PROXY=")
        })
        .collect();

    eprintln!("=== proxy env seen by the agent's bash ===");
    for l in &proxy_env {
        eprintln!("{l}");
    }
    eprintln!("==========================================");

    assert!(
        dump.to_uppercase().contains("NO_PROXY="),
        "agent's bash has no NO_PROXY at all; full proxy env:\n{}",
        proxy_env.join("\n")
    );
    assert!(
        dump.contains("192.168.0.0/16"),
        "agent's bash NO_PROXY is missing the LAN range 192.168.0.0/16 — this is why a LAN \
         git clone goes through the proxy. proxy env seen:\n{}",
        proxy_env.join("\n")
    );
}
