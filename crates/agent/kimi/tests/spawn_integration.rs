//! Integration test: spawn `kimi acp`, verify ACP initialization succeeds,
//! open a session, and shut down cleanly.
//!
//! Requires `kimi` on PATH and a completed `kimi login`.
//! Skipped automatically if `kimi` is not installed.

use agentline_agent_kimi::{KimiConfig, spawn};
use agentline_bridge::AgentBackend;

fn kimi_available() -> bool {
    std::process::Command::new("kimi")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "drives the real `kimi acp` agent; needs `kimi login` (device-code) + network"]
async fn spawn_and_new_session() {
    if !kimi_available() {
        eprintln!("skipping: `kimi` not found on PATH");
        return;
    }

    // Spawn the ACP backend — this starts `kimi acp` and runs the ACP
    // initialize handshake. If --config-file or any other bad flag is
    // passed, the child will exit immediately and `spawn` will fail with
    // "server shut down unexpectedly".
    let backend = spawn(KimiConfig::default())
        .await
        .expect("spawn kimi acp should succeed (is `kimi login` done?)");

    // Open a session — this calls ACP session/new.
    let cwd = std::env::current_dir().expect("cwd");
    let sid = backend
        .new_session(&cwd)
        .await
        .expect("new_session should succeed");

    // Clean up.
    backend
        .close_session(sid)
        .await
        .expect("close_session should succeed");

    backend.shutdown().await;
}
