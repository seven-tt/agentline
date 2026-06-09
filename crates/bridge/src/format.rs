use crate::types::ToolKind;
use std::time::{Duration, SystemTime};

/// Render a tool call as the unified `<emoji> <Name>（<arg>）` label, e.g.
/// `🔧 Shell（find ./x）` or `📖 FileRead（./a.rs）`. The status mark (✅/❌)
/// and session tag are prepended by the caller, keeping status at the front.
pub fn tool_label(kind: ToolKind, arg: &str) -> String {
    format!("{} {}（{}）", kind.emoji(), kind.name(), arg)
}

/// Strip the working-directory prefix from a path-like string, returning a
/// relative path. Non-path strings (commands, urls, queries) pass through unchanged.
pub fn strip_cwd_prefix(s: &str, cwd: &std::path::Path) -> String {
    let cwd_str = cwd.to_string_lossy();
    if s.starts_with(cwd_str.as_ref()) {
        let rest = &s[cwd_str.len()..];
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        if rest.is_empty() { ".".to_string() } else { rest.to_string() }
    } else {
        s.to_string()
    }
}

/// Char-count truncation (not byte). Appends an ellipsis marker if cut.
pub fn truncate(s: &str, max_chars: usize) -> String {
    let mut end = s.len();
    for (count, (i, _)) in s.char_indices().enumerate() {
        if count >= max_chars {
            end = i;
            break;
        }
    }
    if end < s.len() {
        let mut out = s[..end].to_string();
        out.push_str("…(截断)");
        out
    } else {
        s.to_string()
    }
}

/// Format a wall-clock time as local `YYYY-MM-DD HH:MM:SS`.
///
/// Falls back to UTC (with a ` UTC` suffix) if the local offset can't be
/// determined.
pub fn fmt_local(t: SystemTime) -> String {
    use time::OffsetDateTime;

    let utc: OffsetDateTime = t.into();
    let (dt, suffix) = match OffsetDateTime::now_local() {
        Ok(now) => (utc.to_offset(now.offset()), ""),
        Err(_) => (utc, " UTC"),
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}{}",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        suffix
    )
}

/// Human-friendly elapsed time, e.g. `3秒`, `5分钟`, `2小时12分`, `1天3小时`.
pub fn fmt_ago(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}秒")
    } else if secs < 3600 {
        format!("{}分钟", secs / 60)
    } else if secs < 86_400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{h}小时")
        } else {
            format!("{h}小时{m}分")
        }
    } else {
        let days = secs / 86_400;
        let h = (secs % 86_400) / 3600;
        if h == 0 {
            format!("{days}天")
        } else {
            format!("{days}天{h}小时")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_ago_buckets() {
        assert_eq!(fmt_ago(Duration::from_secs(5)), "5秒");
        assert_eq!(fmt_ago(Duration::from_secs(120)), "2分钟");
        assert_eq!(fmt_ago(Duration::from_secs(3700)), "1小时1分");
        assert_eq!(fmt_ago(Duration::from_secs(90_000)), "1天1小时");
    }

    #[test]
    fn fmt_local_shape() {
        // Just check the shape: "YYYY-MM-DD HH:MM:SS".
        let s = fmt_local(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        assert!(s.len() >= 19, "unexpected: {s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], " ");
    }

    #[test]
    fn truncate_unicode() {
        let t = truncate("你好世界你好", 3);
        assert!(t.starts_with("你好世"));
        assert!(t.ends_with("(截断)"));
    }

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }
}
