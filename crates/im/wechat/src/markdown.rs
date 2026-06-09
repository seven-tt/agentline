/// Character-level state machine that strips unsupported Markdown syntax on-the-fly.
///
/// Outputs as much filtered text as possible on each `feed()` call, only holding
/// back the minimum characters needed for pattern disambiguation.
///
/// Constructs passed through (not filtered):
/// - Code fences (```)
/// - Inline code (`)
/// - Tables (|...|)
/// - Horizontal rules (---, ***, ___)
/// - Bold (**)
/// - Italic/bold-italic wrapping non-CJK content
///
/// Constructs filtered (markers stripped, content kept):
/// - Italic/bold-italic wrapping CJK content
/// - Headings H5/H6 (#####, ######)
/// - Images (![alt](url)) — removed entirely
pub struct StreamingMarkdownFilter {
    buf: String,
    fence: bool,
    sol: bool,
    inl: Option<InlineState>,
}

#[derive(Debug, Clone, PartialEq)]
struct InlineState {
    ty: InlineType,
    acc: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InlineType {
    Image,
    Bold3,   // ***
    Italic,  // *
    Ubold3,  // ___
    Uitalic, // _
}

impl StreamingMarkdownFilter {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            fence: false,
            sol: true,
            inl: None,
        }
    }

    /// Feed a new delta and return as much safe output as possible.
    pub fn feed(&mut self, delta: &str) -> String {
        self.buf.push_str(delta);
        self.pump(false)
    }

    /// Flush any remaining buffered content at EOF.
    pub fn flush(&mut self) -> String {
        self.pump(true)
    }

    fn pump(&mut self, eof: bool) -> String {
        let mut out = String::new();
        while !self.buf.is_empty() {
            let s_len = self.buf.len();
            let s_sol = self.sol;
            let s_fence = self.fence;
            let s_inl = self.inl.clone();

            if self.fence {
                out.push_str(&self.pump_fence(eof));
            } else if self.inl.is_some() {
                out.push_str(&self.pump_inline(eof));
            } else if self.sol {
                out.push_str(&self.pump_sol(eof));
            } else {
                out.push_str(&self.pump_body(eof));
            }

            if self.buf.len() == s_len
                && self.sol == s_sol
                && self.fence == s_fence
                && self.inl == s_inl
            {
                break;
            }
        }

        if eof {
            if let Some(ref inl) = self.inl.take() {
                let marker = match inl.ty {
                    InlineType::Image => "![",
                    InlineType::Bold3 => "***",
                    InlineType::Italic => "*",
                    InlineType::Ubold3 => "___",
                    InlineType::Uitalic => "_",
                };
                out.push_str(marker);
                out.push_str(&inl.acc);
            }
        }
        out
    }

    // ── fence ──────────────────────────────────────────────────

    fn pump_fence(&mut self, eof: bool) -> String {
        if self.sol {
            if self.buf.len() < 3 && !eof {
                return String::new();
            }
            if self.buf.starts_with("```") {
                let nl = self.buf.find('\n');
                if let Some(n) = nl {
                    self.fence = false;
                    let line = self.buf.drain(..=n).collect::<String>();
                    self.sol = true;
                    return line;
                }
                if eof {
                    self.fence = false;
                    return std::mem::take(&mut self.buf);
                }
                return String::new();
            }
            self.sol = false;
        }
        if let Some(n) = self.buf.find('\n') {
            let chunk = self.buf.drain(..=n).collect::<String>();
            self.sol = true;
            return chunk;
        }
        std::mem::take(&mut self.buf)
    }

    // ── start-of-line ──────────────────────────────────────────

    fn pump_sol(&mut self, eof: bool) -> String {
        let b = &self.buf;

        if b.starts_with('\n') {
            self.buf.remove(0);
            return "\n".to_string();
        }

        if self.buf.starts_with('`') {
            if self.buf.len() < 3 && !eof {
                return String::new();
            }
            if self.buf.starts_with("```") {
                let nl = self.buf.find('\n');
                if let Some(n) = nl {
                    self.fence = true;
                    let line = self.buf.drain(..=n).collect::<String>();
                    self.sol = true;
                    return line;
                }
                if eof {
                    let rest = std::mem::take(&mut self.buf);
                    return rest;
                }
                return String::new();
            }
            self.sol = false;
            return String::new();
        }

        if self.buf.starts_with('>') {
            self.sol = false;
            return String::new();
        }

        if self.buf.starts_with('#') {
            let n = self.buf.chars().take_while(|&c| c == '#').count();
            if (5..=6).contains(&n) {
                if let Some(rest) = self.buf.get(n..) {
                    if let Some(stripped) = rest.strip_prefix(' ') {
                        self.buf = stripped.to_string();
                        self.sol = false;
                        return String::new();
                    }
                }
            }
            self.sol = false;
            return String::new();
        }

        if self.buf.starts_with(' ') || self.buf.starts_with('\t') {
            if !eof && self.buf.chars().all(|c| c == ' ' || c == '\t') {
                return String::new();
            }
            self.sol = false;
            return String::new();
        }

        if let Some(first) = self.buf.chars().next() {
            if first == '-' || first == '*' || first == '_' {
                let mut bp = 0;
                for (pos, c) in self.buf.char_indices() {
                    if c != first && c != ' ' {
                        break;
                    }
                    bp = pos + c.len_utf8();
                }
                if bp == self.buf.len() && !eof {
                    return String::new();
                }
                if bp < self.buf.len() && self.buf[bp..].starts_with('\n') {
                    let count = self.buf[..bp].chars().filter(|&c| c == first).count();
                    if count >= 3 {
                        let line = self.buf.drain(..bp).collect::<String>();
                        if self.buf.starts_with('\n') {
                            self.buf.remove(0);
                        }
                        self.sol = true;
                        return line;
                    }
                }
                self.sol = false;
                return String::new();
            }
        }

        self.sol = false;
        String::new()
    }

    // ── body ───────────────────────────────────────────────────

    fn pump_body(&mut self, eof: bool) -> String {
        let mut out = String::new();
        let chars: Vec<(usize, char)> = self.buf.char_indices().collect();
        let mut idx = 0;
        while idx < chars.len() {
            let (bp, c) = chars[idx];
            if c == '\n' {
                out.push_str(&self.buf.drain(..bp + c.len_utf8()).collect::<String>());
                self.sol = true;
                return out;
            }
            if c == '!' && idx + 1 < chars.len() && chars[idx + 1].1 == '[' {
                out.push_str(&self.buf.drain(..bp).collect::<String>());
                self.buf.drain(..2); // remove "!["
                self.inl = Some(InlineState {
                    ty: InlineType::Image,
                    acc: String::new(),
                });
                return out;
            }
            if c == '~' {
                idx += 1;
                continue;
            }
            if c == '*' {
                if idx + 2 < chars.len() && chars[idx + 1].1 == '*' && chars[idx + 2].1 == '*' {
                    out.push_str(&self.buf.drain(..bp).collect::<String>());
                    self.buf.drain(..3);
                    self.inl = Some(InlineState {
                        ty: InlineType::Bold3,
                        acc: String::new(),
                    });
                    return out;
                }
                if idx + 1 < chars.len() && chars[idx + 1].1 == '*' {
                    idx += 2;
                    continue;
                }
                if idx + 1 < chars.len() {
                    let next_c = chars[idx + 1].1;
                    if next_c != ' ' && next_c != '\n' {
                        out.push_str(&self.buf.drain(..bp).collect::<String>());
                        self.buf.remove(0); // remove '*'
                        self.inl = Some(InlineState {
                            ty: InlineType::Italic,
                            acc: String::new(),
                        });
                        return out;
                    }
                }
                idx += 1;
                continue;
            }
            if c == '_' {
                if idx + 2 < chars.len() && chars[idx + 1].1 == '_' && chars[idx + 2].1 == '_' {
                    out.push_str(&self.buf.drain(..bp).collect::<String>());
                    self.buf.drain(..3);
                    self.inl = Some(InlineState {
                        ty: InlineType::Ubold3,
                        acc: String::new(),
                    });
                    return out;
                }
                if idx + 1 < chars.len() && chars[idx + 1].1 == '_' {
                    idx += 2;
                    continue;
                }
                if idx + 1 < chars.len() {
                    let next_c = chars[idx + 1].1;
                    if next_c != ' ' && next_c != '\n' {
                        out.push_str(&self.buf.drain(..bp).collect::<String>());
                        self.buf.remove(0); // remove '_'
                        self.inl = Some(InlineState {
                            ty: InlineType::Uitalic,
                            acc: String::new(),
                        });
                        return out;
                    }
                }
                idx += 1;
                continue;
            }
            idx += 1;
        }

        let mut hold = 0;
        if !eof {
            if self.buf.ends_with("**") || self.buf.ends_with("__") {
                hold = 2;
            } else if self.buf.ends_with('*') || self.buf.ends_with('_') || self.buf.ends_with('!')
            {
                hold = 1;
            }
        }
        out.push_str(&self.buf[..self.buf.len().saturating_sub(hold)]);
        self.buf = if hold > 0 {
            self.buf[self.buf.len() - hold..].to_string()
        } else {
            String::new()
        };
        out
    }

    // ── inline ─────────────────────────────────────────────────

    fn pump_inline(&mut self, _eof: bool) -> String {
        let inl = match self.inl.take() {
            Some(s) => s,
            None => return String::new(),
        };
        self.inl = Some(InlineState {
            ty: inl.ty,
            acc: inl.acc + &self.buf,
        });
        self.buf.clear();

        let inl = self.inl.as_mut().unwrap();
        match inl.ty {
            InlineType::Bold3 => {
                if let Some(idx) = inl.acc.find("***") {
                    let content = inl.acc[..idx].to_string();
                    self.buf = inl.acc[idx + 3..].to_string();
                    self.inl = None;
                    if contains_cjk(&content) {
                        return content;
                    }
                    return format!("***{content}***");
                }
                String::new()
            }
            InlineType::Ubold3 => {
                if let Some(idx) = inl.acc.find("___") {
                    let content = inl.acc[..idx].to_string();
                    self.buf = inl.acc[idx + 3..].to_string();
                    self.inl = None;
                    if contains_cjk(&content) {
                        return content;
                    }
                    return format!("___{content}___");
                }
                String::new()
            }
            InlineType::Italic => {
                for (j, ch) in inl.acc.char_indices() {
                    if ch == '\n' {
                        let r = format!("*{}", &inl.acc[..j + 1]);
                        self.buf = inl.acc[j + 1..].to_string();
                        self.inl = None;
                        self.sol = true;
                        return r;
                    }
                    if ch == '*' {
                        if inl.acc.get(j + 1..j + 2) == Some("*") {
                            continue;
                        }
                        let content = inl.acc[..j].to_string();
                        self.buf = inl.acc[j + 1..].to_string();
                        self.inl = None;
                        if contains_cjk(&content) {
                            return content;
                        }
                        return format!("*{content}*");
                    }
                }
                String::new()
            }
            InlineType::Uitalic => {
                for (j, ch) in inl.acc.char_indices() {
                    if ch == '\n' {
                        let r = format!("_{}", &inl.acc[..j + 1]);
                        self.buf = inl.acc[j + 1..].to_string();
                        self.inl = None;
                        self.sol = true;
                        return r;
                    }
                    if ch == '_' {
                        if inl.acc.get(j + 1..j + 2) == Some("_") {
                            continue;
                        }
                        let content = inl.acc[..j].to_string();
                        self.buf = inl.acc[j + 1..].to_string();
                        self.inl = None;
                        if contains_cjk(&content) {
                            return content;
                        }
                        return format!("_{content}_");
                    }
                }
                String::new()
            }
            InlineType::Image => {
                let cb = inl.acc.find(']');
                if cb.is_none() {
                    return String::new();
                }
                let cb = cb.unwrap();
                if cb + 1 >= inl.acc.len() {
                    return String::new();
                }
                if inl.acc.get(cb + 1..cb + 2) != Some("(") {
                    let r = format!("![{}", &inl.acc[..cb + 1]);
                    self.buf = inl.acc[cb + 1..].to_string();
                    self.inl = None;
                    return r;
                }
                if let Some(cp) = inl.acc[cb + 2..].find(')') {
                    self.buf = inl.acc[cb + 2 + cp + 1..].to_string();
                    self.inl = None;
                    return String::new();
                }
                String::new()
            }
        }
    }
}

impl Default for StreamingMarkdownFilter {
    fn default() -> Self {
        Self::new()
    }
}

fn contains_cjk(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c,
            '\u{2E80}'..='\u{9FFF}' | '\u{AC00}'..='\u{D7AF}' | '\u{F900}'..='\u{FAFF}'
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_plain() {
        let mut f = StreamingMarkdownFilter::new();
        assert_eq!(f.feed("hello world"), "hello world");
        assert_eq!(f.flush(), "");
    }

    #[test]
    fn strips_cjk_italic() {
        let mut f = StreamingMarkdownFilter::new();
        assert_eq!(f.feed("*中文*"), "中文");
        assert_eq!(f.flush(), "");
    }

    #[test]
    fn keeps_non_cjk_italic() {
        let mut f = StreamingMarkdownFilter::new();
        assert_eq!(f.feed("*hello*"), "*hello*");
        assert_eq!(f.flush(), "");
    }

    #[test]
    fn strips_h5_heading() {
        let mut f = StreamingMarkdownFilter::new();
        assert_eq!(f.feed("##### title\n"), "title\n");
    }

    #[test]
    fn removes_image() {
        let mut f = StreamingMarkdownFilter::new();
        assert_eq!(f.feed("![alt](url)"), "");
        assert_eq!(f.flush(), "");
    }

    #[test]
    fn code_fence_passthrough() {
        let mut f = StreamingMarkdownFilter::new();
        let out = f.feed("```rust\ncode\n```\n");
        assert_eq!(out, "```rust\ncode\n```\n");
    }
}
