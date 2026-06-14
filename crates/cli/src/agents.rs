use crate::config::AgentSection;

pub struct AgentMeta {
    pub id: &'static str,
    pub cli_cmd: &'static str,
    pub install: Option<&'static [&'static str]>,
    pub auth_check: fn(&AgentSection) -> bool,
}

impl AgentMeta {
    pub fn status(&self, installed: bool, cfg: &AgentSection) -> &'static str {
        if !installed {
            return "not_installed";
        }
        if (self.auth_check)(cfg) {
            "ready"
        } else {
            "needs_login"
        }
    }
}

pub fn has_home_file(segments: &[&str]) -> bool {
    if let Some(home) = dirs::home_dir() {
        let mut p = home;
        for s in segments {
            p.push(s);
        }
        p.exists()
    } else {
        false
    }
}

fn check_claude_settings_auth() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let path = home.join(".claude").join("settings.json");
    let Ok(data) = std::fs::read(&path) else {
        return false;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data) else {
        return false;
    };
    let Some(env) = v.get("env").and_then(|e| e.as_object()) else {
        return false;
    };
    env.get("ANTHROPIC_API_KEY")
        .or_else(|| env.get("ANTHROPIC_AUTH_TOKEN"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

fn check_claude_login() -> bool {
    std::process::Command::new("claude")
        .args(["auth", "status"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .is_some_and(|v| v.get("loggedIn") == Some(&serde_json::Value::Bool(true)))
}

fn check_qoder_login() -> bool {
    std::process::Command::new("qodercli")
        .arg("status")
        .output()
        .ok()
        .is_some_and(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            s.contains("Account:") && !s.contains("Not logged in")
        })
}

fn check_kiro_login() -> bool {
    std::process::Command::new("kiro-cli")
        .arg("whoami")
        .output()
        .ok()
        .is_some_and(|o| o.status.success())
}

pub const AGENTS: &[AgentMeta] = &[
    AgentMeta {
        id: "claude-code",
        cli_cmd: "claude",
        install: Some(&["npm", "install", "-g", "@anthropic-ai/claude-code"]),
        auth_check: |_| {
            std::env::var("ANTHROPIC_API_KEY").is_ok_and(|v| !v.is_empty())
                || std::env::var("ANTHROPIC_AUTH_TOKEN").is_ok_and(|v| !v.is_empty())
                || check_claude_settings_auth()
                || check_claude_login()
        },
    },
    AgentMeta {
        id: "kimi",
        cli_cmd: "kimi",
        install: Some(&[
            "sh",
            "-c",
            "curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash",
        ]),
        auth_check: |c| {
            !c.kimi.access_token.is_empty()
                || has_home_file(&[".kimi-code", "credentials", "kimi-code.json"])
                || has_home_file(&[".kimi", "credentials", "kimi-code.json"])
        },
    },
    AgentMeta {
        id: "qoder",
        cli_cmd: "qodercli",
        install: Some(&["sh", "-c", "curl -fsSL https://qoder.com/install | bash"]),
        auth_check: |c| !c.qoder.personal_access_token.is_empty() || check_qoder_login(),
    },
    AgentMeta {
        id: "opencode",
        cli_cmd: "opencode",
        install: Some(&["npm", "install", "-g", "opencode-ai"]),
        auth_check: |c| {
            !c.opencode.api_key.is_empty()
                || has_home_file(&[".local", "share", "opencode", "auth.json"])
        },
    },
    AgentMeta {
        id: "kiro",
        cli_cmd: "kiro-cli",
        install: Some(&["sh", "-c", "curl -fsSL https://cli.kiro.dev/install | bash"]),
        auth_check: |_| {
            std::env::var("KIRO_API_KEY").is_ok_and(|v| !v.is_empty()) || check_kiro_login()
        },
    },
    AgentMeta {
        id: "gemini",
        cli_cmd: "gemini",
        install: Some(&["npm", "install", "-g", "@google/gemini-cli"]),
        auth_check: |c| {
            !c.gemini.api_key.is_empty()
                || std::env::var("GEMINI_API_KEY").is_ok_and(|v| !v.is_empty())
                || std::env::var("GOOGLE_API_KEY").is_ok_and(|v| !v.is_empty())
        },
    },
    AgentMeta {
        id: "hermes",
        cli_cmd: "hermes",
        install: Some(&[
            "sh",
            "-c",
            "curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash",
        ]),
        auth_check: |_| has_home_file(&[".hermes", "auth.json"]),
    },
    AgentMeta {
        id: "codex",
        cli_cmd: "codex",
        install: Some(&["npm", "install", "-g", "@openai/codex"]),
        auth_check: |c| {
            !c.codex.api_key.is_empty()
                || has_home_file(&[".codex", "auth.json"])
                || std::env::var("OPENAI_API_KEY").is_ok_and(|v| !v.is_empty())
        },
    },
    AgentMeta {
        id: "acp",
        cli_cmd: "",
        install: None,
        auth_check: |c| !c.acp.command.is_empty(),
    },
];

pub fn detect_agent(cmd: &str) -> (bool, Option<String>) {
    if cmd.is_empty() {
        return (false, None);
    }
    let installed = which::which(cmd).is_ok();
    let version = if installed {
        std::process::Command::new(cmd)
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if s.is_empty() {
                    String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .next()
                        .map(|l| l.trim().to_string())
                } else {
                    Some(
                        s.lines()
                            .next()
                            .unwrap_or(&s)
                            .trim()
                            .rsplit(' ')
                            .next()
                            .unwrap_or(&s)
                            .trim_start_matches('v')
                            .to_string(),
                    )
                }
            })
    } else {
        None
    };
    (installed, version)
}
