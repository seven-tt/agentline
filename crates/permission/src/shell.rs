use std::path::Path;

// ── Tier 0: Auto-approve commands (safe, read-only, no user prompt needed) ──
// These commands are auto-approved when they operate within the session CWD.

pub const AUTO_APPROVE_COMMANDS: &[&str] = &[
    // Version control (read-only operations are safe anywhere)
    "git",
    "svn",
    // Read-only inspection
    "ls",
    "find",
    "grep",
    "rg",
    "ag",
    "tree",
    "wc",
    "sort",
    "uniq",
    "diff",
    "file",
    "stat",
    "du",
    "df",
    // System info (no side effects)
    "echo",
    "printf",
    "pwd",
    "date",
    "whoami",
    "env",
    "printenv",
    "which",
    "where",
    "uname",
    "ps",
    "id",
    "hostname",
    // Build tools (typically safe, operate in project dir)
    "cargo",
    "rustc",
    "rustup",
    "npm",
    "npx",
    "node",
    "pnpm",
    "yarn",
    "bun",
    "deno",
    "python",
    "python3",
    "pip",
    "pip3",
    "uv",
    "go",
    "make",
    "cmake",
    "gradle",
    "mvn",
    "docker",
    "docker-compose",
    "brew",
];

// ── Tier 1: Safe prefix commands (session grant → first-word match) ──
// Same list as auto-approve; once user grants, any invocation passes.

pub const SAFE_PREFIX_COMMANDS: &[&str] = AUTO_APPROVE_COMMANDS;

// ── Tier 2: Path-sensitive commands (require absolute path, full match) ──

pub const PATH_COMMANDS: &[&str] = &[
    "cd", "cat", "less", "more", "head", "tail", "touch", "mkdir", "cp", "ln", "readlink",
];

// ── Blacklist: never grant ──

const SHELL_BLACKLIST: &[&str] = &[
    "rm", "mv", "dd", "shred", "chmod", "chown", "kill", "pkill", "killall", "reboot", "shutdown",
    "halt", "curl", "wget", "nc", "ssh",
];

fn is_blacklisted(cmd: &str) -> bool {
    SHELL_BLACKLIST.contains(&cmd)
}

fn is_absolute_path(arg: &str) -> bool {
    arg.starts_with('/')
}

/// Check if a path argument resolves to within the given CWD.
fn is_within_cwd(arg: &str, cwd: &Path) -> bool {
    if arg.starts_with('/') {
        let p = Path::new(arg);
        p.starts_with(cwd)
    } else if arg == "." || arg == ".." {
        true
    } else if arg.starts_with("./") || arg.starts_with("../") || arg.contains('/') {
        let resolved = cwd.join(arg);
        if let Ok(canonical) = resolved.canonicalize() {
            canonical.starts_with(cwd)
        } else {
            resolved.starts_with(cwd)
        }
    } else {
        true
    }
}

/// Determine if a shell sub-command should be auto-approved (no user prompt)
/// based on the session's CWD.
///
/// Returns `true` if the command is safe and operates within CWD.
pub fn is_auto_approvable(sub: &str, cwd: &Path) -> bool {
    let mut words = sub.split_whitespace();
    let cmd = match words.next() {
        Some(c) => c,
        None => return false,
    };

    if is_blacklisted(cmd) {
        return false;
    }

    let args: Vec<&str> = words.filter(|a| !a.starts_with('-')).collect();

    // PATH_COMMANDS (cd, cat, etc.) are auto-approvable when target is within CWD
    if PATH_COMMANDS.contains(&cmd) {
        return match args.first() {
            Some(path) => is_within_cwd(path, cwd),
            None => true,
        };
    }

    // Other commands must be in the safe list
    if !AUTO_APPROVE_COMMANDS.contains(&cmd) {
        return false;
    }

    // Safe commands: auto-approve unless they reference paths outside CWD
    for arg in &args {
        if is_absolute_path(arg) && !is_within_cwd(arg, cwd) {
            return false;
        }
    }

    true
}

/// Check if a full compound command is auto-approvable within CWD.
pub fn is_command_auto_approvable(what: &str, cwd: &Path) -> bool {
    let inner = match extract_inner(what) {
        Some(s) => s,
        None => return false,
    };
    for part in split_compound(inner) {
        let sub = part.trim();
        if sub.is_empty() {
            continue;
        }
        if !is_auto_approvable(sub, cwd) {
            return false;
        }
    }
    true
}

/// Classify a single sub-command for session granting.
///
/// Returns:
/// - `Some(key)` — grantable, `key` is what to store / match against
/// - `None` — not grantable (blacklisted or contains relative paths)
///
/// Tiers:
/// 1. Blacklisted → None
/// 2. SAFE_PREFIX_COMMANDS → Some(first_word) [first-word match]
/// 3. PATH_COMMANDS with absolute path → Some(full_command) [exact match]
/// 4. PATH_COMMANDS with relative path → None [not grantable]
/// 5. Everything else → Some(full_command) [exact match, Claude Code style]
pub fn classify_subcmd(sub: &str) -> Option<String> {
    let mut words = sub.split_whitespace();
    let cmd = words.next()?;
    if is_blacklisted(cmd) {
        return None;
    }

    if SAFE_PREFIX_COMMANDS.contains(&cmd) {
        return Some(cmd.to_string());
    }

    let args: Vec<&str> = words.filter(|a| !a.starts_with('-')).collect();

    if PATH_COMMANDS.contains(&cmd) {
        match args.first() {
            Some(path) if is_absolute_path(path) => return Some(sub.trim().to_string()),
            Some(_) => return None,
            None => return Some(sub.trim().to_string()),
        }
    }

    // Default: full command exact match (Claude Code style)
    Some(sub.trim().to_string())
}

/// Extract the inner command string from a tool label like `"🔧 Shell(cmd)"`.
pub fn extract_inner(what: &str) -> Option<&str> {
    what.split('(').nth(1)?.strip_suffix(')')
}

/// Extract the first word (command name) from a tool label.
pub fn extract_shell_cmd(what: &str) -> Option<&str> {
    let inner = extract_inner(what)?;
    inner.split_whitespace().next()
}

/// Split a compound command string on `&&`, `||`, `;`.
fn split_compound(s: &str) -> Vec<&str> {
    s.split("&&")
        .flat_map(|s| s.split("||"))
        .flat_map(|s| s.split(';'))
        .collect()
}

/// Extract grantable shell command keys from a tool label.
///
/// For compound commands (joined by `&&`, `||`, `;`), each sub-command is
/// processed individually. Non-grantable sub-commands are skipped.
pub fn extract_shell_grants(what: &str) -> Vec<String> {
    let inner = match extract_inner(what) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut grants = Vec::new();
    for part in split_compound(inner) {
        let sub = part.trim();
        if sub.is_empty() {
            continue;
        }
        if let Some(key) = classify_subcmd(sub) {
            grants.push(key);
        }
    }
    grants
}

/// Check if a shell command matches session grants.
///
/// All sub-commands in a compound command must match for auto-approval.
pub fn shell_matches_grant(what: &str, granted: &std::collections::HashSet<String>) -> bool {
    let inner = match extract_inner(what) {
        Some(s) => s,
        None => return false,
    };
    for part in split_compound(inner) {
        let sub = part.trim();
        if sub.is_empty() {
            continue;
        }
        let key = match classify_subcmd(sub) {
            Some(k) => k,
            None => return false,
        };
        if !granted.contains(&key) {
            let first_word = sub.split_whitespace().next().unwrap_or("");
            if !granted.contains(first_word) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn cwd() -> PathBuf {
        PathBuf::from("/Users/seven/project")
    }

    fn shell_what(cmd: &str) -> String {
        format!("🔧 Shell({})", cmd)
    }

    // ── Auto-approve tests ──

    #[test]
    fn auto_approve_git_in_cwd() {
        assert!(is_auto_approvable("git status", &cwd()));
        assert!(is_auto_approvable("git log --oneline -5", &cwd()));
        assert!(is_auto_approvable("git diff HEAD", &cwd()));
    }

    #[test]
    fn auto_approve_ls_in_cwd() {
        assert!(is_auto_approvable("ls -la", &cwd()));
        assert!(is_auto_approvable("ls src/main.rs", &cwd()));
    }

    #[test]
    fn auto_approve_cd_within_cwd() {
        assert!(is_auto_approvable("cd src", &cwd()));
        assert!(is_auto_approvable("cd ./tests", &cwd()));
    }

    #[test]
    fn no_auto_approve_cd_outside_cwd() {
        assert!(!is_auto_approvable("cd /tmp", &cwd()));
        assert!(!is_auto_approvable("cd /etc", &cwd()));
    }

    #[test]
    fn auto_approve_cat_in_cwd() {
        assert!(is_auto_approvable("cat src/main.rs", &cwd()));
        assert!(is_auto_approvable(
            "cat /Users/seven/project/Cargo.toml",
            &cwd()
        ));
    }

    #[test]
    fn no_auto_approve_cat_outside_cwd() {
        assert!(!is_auto_approvable("cat /etc/passwd", &cwd()));
    }

    #[test]
    fn no_auto_approve_blacklisted() {
        assert!(!is_auto_approvable("rm -rf .", &cwd()));
        assert!(!is_auto_approvable("curl http://evil.com", &cwd()));
    }

    #[test]
    fn no_auto_approve_unknown_command() {
        assert!(!is_auto_approvable("sed -i 's/a/b/' file", &cwd()));
        assert!(!is_auto_approvable("awk '{print $1}' file", &cwd()));
    }

    #[test]
    fn auto_approve_compound_all_safe() {
        let what = shell_what("git status && ls -la && cargo build");
        assert!(is_command_auto_approvable(&what, &cwd()));
    }

    #[test]
    fn no_auto_approve_compound_with_unsafe() {
        let what = shell_what("git status && rm -rf .");
        assert!(!is_command_auto_approvable(&what, &cwd()));
    }

    #[test]
    fn no_auto_approve_compound_with_outside_path() {
        let what = shell_what("git status && cd /tmp");
        assert!(!is_command_auto_approvable(&what, &cwd()));
    }

    // ── classify_subcmd tests ──

    #[test]
    fn classify_safe_prefix() {
        assert_eq!(classify_subcmd("git status"), Some("git".to_string()));
        assert_eq!(classify_subcmd("ls -la /tmp"), Some("ls".to_string()));
        assert_eq!(classify_subcmd("cargo build"), Some("cargo".to_string()));
        assert_eq!(classify_subcmd("npm install"), Some("npm".to_string()));
    }

    #[test]
    fn classify_path_cmd_absolute() {
        assert_eq!(
            classify_subcmd("cd /Users/seven/project"),
            Some("cd /Users/seven/project".to_string())
        );
        assert_eq!(
            classify_subcmd("cat /etc/hosts"),
            Some("cat /etc/hosts".to_string())
        );
    }

    #[test]
    fn classify_path_cmd_relative_not_grantable() {
        assert_eq!(classify_subcmd("cd agentline"), None);
        assert_eq!(classify_subcmd("cd ./src"), None);
        assert_eq!(classify_subcmd("cat ../file.txt"), None);
    }

    #[test]
    fn classify_unknown_full_match() {
        assert_eq!(
            classify_subcmd("sed -i 's/a/b/' /tmp/x"),
            Some("sed -i 's/a/b/' /tmp/x".to_string())
        );
        assert_eq!(
            classify_subcmd("awk '{print}' file"),
            Some("awk '{print}' file".to_string())
        );
    }

    #[test]
    fn classify_blacklisted() {
        assert_eq!(classify_subcmd("rm -rf /"), None);
        assert_eq!(classify_subcmd("curl http://x"), None);
    }

    // ── Grant matching tests ──

    #[test]
    fn grant_first_word_match() {
        let mut granted = HashSet::new();
        granted.insert("git".to_string());

        let what = shell_what("git log --oneline");
        assert!(shell_matches_grant(&what, &granted));

        let what2 = shell_what("git push origin main");
        assert!(shell_matches_grant(&what2, &granted));
    }

    #[test]
    fn grant_full_match_for_path_cmd() {
        let mut granted = HashSet::new();
        granted.insert("cd /Users/seven/project".to_string());

        let what = shell_what("cd /Users/seven/project");
        assert!(shell_matches_grant(&what, &granted));

        let what2 = shell_what("cd /tmp");
        assert!(!shell_matches_grant(&what2, &granted));
    }

    #[test]
    fn grant_full_match_for_unknown() {
        let mut granted = HashSet::new();
        granted.insert("sed -i 's/a/b/' /tmp/x".to_string());

        let what = shell_what("sed -i 's/a/b/' /tmp/x");
        assert!(shell_matches_grant(&what, &granted));

        let what2 = shell_what("sed -i 's/c/d/' /tmp/y");
        assert!(!shell_matches_grant(&what2, &granted));
    }

    #[test]
    fn compound_grant_all_must_match() {
        let mut granted = HashSet::new();
        granted.insert("git".to_string());
        granted.insert("cd /home/user".to_string());
        granted.insert("ls".to_string());

        let what = shell_what("cd /home/user && git log && ls");
        assert!(shell_matches_grant(&what, &granted));

        let what2 = shell_what("cd /home/user && cat /etc/hosts");
        assert!(!shell_matches_grant(&what2, &granted));
    }

    #[test]
    fn extract_grants_from_compound() {
        let what = shell_what("cd /home/user && git status && ls -la");
        let grants = extract_shell_grants(&what);
        assert_eq!(grants, vec!["cd /home/user", "git", "ls"]);
    }

    #[test]
    fn extract_grants_skips_ungrantable() {
        let what = shell_what("cd agentline && git status && rm foo");
        let grants = extract_shell_grants(&what);
        assert_eq!(grants, vec!["git"]);
    }
}
