use agentline_bridge::types::Command;
use std::path::PathBuf;

/// Parse a text message into a Command. Convenience helper for text-based
/// InputSources (IM platforms). Non-text sources produce Commands directly.
pub fn parse_text(text: &str) -> Command {
    let trimmed = text.trim();

    if let Some(rest) = trimmed.strip_prefix('/') {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let head = parts.next().unwrap_or("").to_ascii_lowercase();
        let arg = parts.next().unwrap_or("").trim();
        return match head.as_str() {
            "cd" if !arg.is_empty() => Command::Cd(expand_tilde(arg)),
            "cd" => Command::CdInteractive,
            "new" => Command::New,
            "close" | "stop" => {
                let id = arg
                    .strip_prefix('#')
                    .and_then(|n| n.parse::<u32>().ok())
                    .or_else(|| arg.parse::<u32>().ok());
                Command::Close(id)
            }
            "cancel" => Command::Cancel,
            "agent" => {
                if arg.is_empty() {
                    Command::Agent(None)
                } else {
                    Command::Agent(Some(arg.to_string()))
                }
            }
            "sessions" | "session" | "ls" => Command::Sessions,
            "yolo" => Command::Yolo,
            "safe" => Command::Safe,
            "help" | "?" => Command::Help,
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
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    if p == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands() {
        assert_eq!(parse_text("/help"), Command::Help);
        assert_eq!(parse_text("/?"), Command::Help);
        assert_eq!(parse_text("/new"), Command::New);
        assert_eq!(parse_text("/close"), Command::Close(None));
        assert_eq!(parse_text("/close #1"), Command::Close(Some(1)));
        assert_eq!(parse_text("/close #42"), Command::Close(Some(42)));
        assert_eq!(parse_text("/stop"), Command::Close(None));
        assert_eq!(parse_text("/stop #1"), Command::Close(Some(1)));
        assert_eq!(parse_text("/cancel"), Command::Cancel);
        assert_eq!(parse_text("/agent"), Command::Agent(None));
        assert_eq!(
            parse_text("/agent kimi"),
            Command::Agent(Some("kimi".to_string()))
        );
        assert_eq!(parse_text("/yolo"), Command::Yolo);
        assert_eq!(parse_text("/safe"), Command::Safe);
        assert_eq!(parse_text("/sessions"), Command::Sessions);
        assert_eq!(parse_text("/ls"), Command::Sessions);
        assert_eq!(parse_text("/HELP"), Command::Help);
    }

    #[test]
    fn unknown_slash_returns_help() {
        assert_eq!(parse_text("/compact"), Command::Help);
        assert_eq!(parse_text("/init"), Command::Help);
        assert_eq!(parse_text("/clear"), Command::Help);
        assert_eq!(parse_text("/xyz foo bar"), Command::Help);
    }

    #[test]
    fn cd_with_path() {
        assert_eq!(parse_text("/cd /tmp"), Command::Cd(PathBuf::from("/tmp")));
        assert_eq!(
            parse_text("  /cd   /tmp/a  "),
            Command::Cd(PathBuf::from("/tmp/a"))
        );
    }

    #[test]
    fn cd_without_arg_is_interactive() {
        assert_eq!(parse_text("/cd"), Command::CdInteractive);
    }

    #[test]
    fn yes_no_session_tokens() {
        assert_eq!(parse_text("y"), Command::YesToken);
        assert_eq!(parse_text("YES"), Command::YesToken);
        assert_eq!(parse_text("是"), Command::YesToken);
        assert_eq!(parse_text("n"), Command::NoToken);
        assert_eq!(parse_text("否"), Command::NoToken);
        assert_eq!(parse_text("s"), Command::SessionApprove);
        assert_eq!(parse_text("S"), Command::SessionApprove);
        assert_eq!(parse_text("session"), Command::SessionApprove);
    }

    #[test]
    fn plain_text() {
        assert_eq!(
            parse_text("hello world"),
            Command::Plain("hello world".into())
        );
        assert_eq!(parse_text("  trim me  "), Command::Plain("trim me".into()));
        assert_eq!(parse_text("yep"), Command::Plain("yep".into()));
    }
}
