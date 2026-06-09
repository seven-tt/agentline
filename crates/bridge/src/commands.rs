use std::path::PathBuf;

/// What a single inbound text resolves to *syntactically*. The bridge decides
/// the *semantic* interpretation (e.g. "y" is a confirmation only when a
/// permission request is pending; otherwise it's a regular prompt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `/cd <path>` — with explicit path
    Cd(PathBuf),
    /// `/cd` — no argument, trigger interactive directory listing
    CdInteractive,
    /// `/new`
    New,
    /// `/stop [#N]` — optional session short-id (digits after `#`).
    Stop(Option<u32>),
    /// `/sessions` — list active sessions.
    Sessions,
    /// `/yolo`
    Yolo,
    /// `/safe`
    Safe,
    /// `/help` or `/?`
    Help,
    /// Bare-token affirmative: `y`, `yes`, `是`, `好`.
    YesToken,
    /// Bare-token negative: `n`, `no`, `否`, `不`.
    NoToken,
    /// Session-level approval: `s`, `session`, `会话`.
    SessionApprove,
    /// Anything else — including unknown slash-commands which are forwarded
    /// to the agent as-is.
    Plain(String),
}

pub fn parse(text: &str) -> Command {
    let trimmed = text.trim();

    if let Some(rest) = trimmed.strip_prefix('/') {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let head = parts.next().unwrap_or("").to_ascii_lowercase();
        let arg = parts.next().unwrap_or("").trim();
        return match head.as_str() {
            "cd" if !arg.is_empty() => Command::Cd(expand_tilde(arg)),
            "cd" => Command::CdInteractive,
            "new" => Command::New,
            "stop" | "cancel" => {
                // Optional "#N" argument.
                let id = arg
                    .strip_prefix('#')
                    .and_then(|n| n.parse::<u32>().ok())
                    .or_else(|| arg.parse::<u32>().ok());
                Command::Stop(id)
            }
            "sessions" | "session" | "ls" => Command::Sessions,
            "yolo" => Command::Yolo,
            "safe" => Command::Safe,
            "help" | "?" => Command::Help,
            // Unknown slash-commands return the help text.
            _ => Command::Help,
        };
    }

    let lower = trimmed.to_ascii_lowercase();
    if matches_token(&lower, &["y", "yes", "是", "好", "ok", "对"]) {
        return Command::YesToken;
    }
    if matches_token(&lower, &["n", "no", "否", "不", "取消"]) {
        return Command::NoToken;
    }
    if matches_token(&lower, &["s", "session", "会话"]) {
        return Command::SessionApprove;
    }

    Command::Plain(trimmed.to_string())
}

fn matches_token(s: &str, options: &[&str]) -> bool {
    options.contains(&s)
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut buf = PathBuf::from(home);
            buf.push(rest);
            return buf;
        }
    }
    if p == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands() {
        assert_eq!(parse("/help"), Command::Help);
        assert_eq!(parse("/?"), Command::Help);
        assert_eq!(parse("/new"), Command::New);
        assert_eq!(parse("/stop"), Command::Stop(None));
        assert_eq!(parse("/stop #1"), Command::Stop(Some(1)));
        assert_eq!(parse("/stop #42"), Command::Stop(Some(42)));
        assert_eq!(parse("/cancel"), Command::Stop(None));
        assert_eq!(parse("/yolo"), Command::Yolo);
        assert_eq!(parse("/safe"), Command::Safe);
        assert_eq!(parse("/sessions"), Command::Sessions);
        assert_eq!(parse("/ls"), Command::Sessions);
        assert_eq!(parse("/HELP"), Command::Help);
    }

    #[test]
    fn unknown_slash_returns_help() {
        assert_eq!(parse("/compact"), Command::Help);
        assert_eq!(parse("/init"), Command::Help);
        assert_eq!(parse("/clear"), Command::Help);
        assert_eq!(parse("/xyz foo bar"), Command::Help);
    }

    #[test]
    fn cd_with_path() {
        assert_eq!(parse("/cd /tmp"), Command::Cd(PathBuf::from("/tmp")));
        assert_eq!(
            parse("  /cd   /tmp/a  "),
            Command::Cd(PathBuf::from("/tmp/a"))
        );
    }

    #[test]
    fn cd_without_arg_is_interactive() {
        assert_eq!(parse("/cd"), Command::CdInteractive);
    }

    #[test]
    fn yes_no_session_tokens() {
        assert_eq!(parse("y"), Command::YesToken);
        assert_eq!(parse("YES"), Command::YesToken);
        assert_eq!(parse("是"), Command::YesToken);
        assert_eq!(parse("n"), Command::NoToken);
        assert_eq!(parse("否"), Command::NoToken);
        assert_eq!(parse("s"), Command::SessionApprove);
        assert_eq!(parse("S"), Command::SessionApprove);
        assert_eq!(parse("session"), Command::SessionApprove);
    }

    #[test]
    fn plain_text() {
        assert_eq!(parse("hello world"), Command::Plain("hello world".into()));
        assert_eq!(parse("  trim me  "), Command::Plain("trim me".into()));
        assert_eq!(parse("yep"), Command::Plain("yep".into()));
    }
}
