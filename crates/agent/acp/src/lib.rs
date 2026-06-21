//! Generic ACP (Agent Client Protocol) backend for agentline.
//!
//! This crate is now a thin wrapper:
//! - The driving loop (`bridge_main`) and `AcpBackend` live in `agentline_bridge::driver`.
//! - This crate provides `AcpBackendConfig` (implements `AcpCodec`) so agent crates
//!   can build a backend without depending on `agent_client_protocol` directly.

pub mod plugin;
pub mod raw_log;

pub use plugin::plugin;

// Re-export the key types from bridge so downstream crates keep the same import paths.
pub use agent_client_protocol::ToolCallUpdate;
pub use agent_client_protocol::{McpServer, McpServerHttp};
pub use agentline_bridge::driver::{AcpBackend, AcpCodec, ToolCallParser};

use agentline_bridge::transport::SpawnSpec;
use std::path::PathBuf;
use std::sync::Arc;

/// Configuration for a generic ACP agent subprocess.
///
/// Implements [`AcpCodec`] so it can be passed directly to [`AcpBackend::spawn`].
#[derive(Debug, Clone, Default)]
pub struct AcpBackendConfig {
    pub command: String,
    pub args: Vec<String>,
    /// Extra env vars injected into the child (applied after `remove_env`).
    pub extra_env: Vec<(String, String)>,
    /// Env vars stripped from the child environment.
    pub remove_env: Vec<String>,
    /// If set, the child PID is written here on spawn and removed on shutdown.
    pub pid_file: Option<PathBuf>,
    /// Optional per-agent tool-call normaliser.
    pub parser: Option<Arc<dyn ToolCallParser>>,
    /// MCP servers to inject into every new ACP session.
    pub mcp_servers: Vec<agent_client_protocol::McpServer>,
}

impl AcpBackendConfig {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            ..Default::default()
        }
    }
}

impl AcpCodec for AcpBackendConfig {
    fn spawn_spec(&self) -> SpawnSpec {
        SpawnSpec {
            command: self.command.clone(),
            args: self.args.clone(),
            extra_env: self.extra_env.clone(),
            remove_env: self.remove_env.clone(),
        }
    }

    fn pid_file(&self) -> Option<PathBuf> {
        self.pid_file.clone()
    }

    fn tool_call_parser(&self) -> Option<Arc<dyn ToolCallParser>> {
        self.parser.clone()
    }

    fn mcp_servers(&self) -> Vec<agent_client_protocol::McpServer> {
        self.mcp_servers.clone()
    }
}

pub use agentline_bridge::process::cleanup_orphaned_agent;

#[cfg(test)]
mod proxy_env_tests {
    use agentline_bridge::transport::SpawnSpec;
    use tokio::io::AsyncReadExt;

    async fn spawn_and_capture(shell_cmd: &str) -> String {
        let spec = SpawnSpec {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), shell_cmd.into()],
            extra_env: vec![],
            remove_env: vec![],
        };
        let mut child = agentline_bridge::transport::spawn(&spec).expect("spawn child");
        let mut out = String::new();
        child
            .stdout
            .take()
            .expect("piped stdout")
            .read_to_string(&mut out)
            .await
            .expect("read stdout");
        let _ = child.wait().await;
        out
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_child_inherits_injected_no_proxy() {
        let out = spawn_and_capture(r#"printf '%s' "$NO_PROXY""#).await;
        assert!(
            out.contains("192.168.0.0/16"),
            "direct child NO_PROXY missing LAN range, got: {out:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nested_shell_grandchild_inherits_no_proxy() {
        let out = spawn_and_capture(r#"/bin/sh -c 'printf "%s" "$NO_PROXY"'"#).await;
        assert!(
            out.contains("192.168.0.0/16"),
            "nested-shell grandchild NO_PROXY missing LAN range, got: {out:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kills_whole_process_group() {
        use agentline_bridge::process::kill_process_group;
        use std::time::Duration;

        let spec = SpawnSpec {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 300 & sleep 300".into()],
            extra_env: vec![],
            remove_env: vec![],
        };
        let mut child = agentline_bridge::transport::spawn(&spec).expect("spawn");
        let pid = child.id().expect("child pid") as i32;

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            unsafe { libc::kill(-pid, 0) },
            0,
            "process group should be alive before kill"
        );

        kill_process_group(pid);
        let _ = child.wait().await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert_ne!(
            unsafe { libc::kill(-pid, 0) },
            0,
            "process group {pid} still alive after kill_process_group"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn kills_whole_session() {
        use agentline_bridge::process::kill_session;
        use std::time::Duration;

        let spec = SpawnSpec {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 300 & sleep 300".into()],
            extra_env: vec![],
            remove_env: vec![],
        };
        let mut child = agentline_bridge::transport::spawn(&spec).expect("spawn");
        let pid = child.id().expect("child pid") as i32;

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            unsafe { libc::getsid(pid) },
            pid,
            "spawn should put the child in its own session"
        );

        kill_session(pid);
        let _ = child.wait().await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert_ne!(
            unsafe { libc::kill(-pid, 0) },
            0,
            "session {pid} still alive after kill_session"
        );
    }
}
