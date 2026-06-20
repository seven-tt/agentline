//! Convert standard Markdown to Telegram MarkdownV2 format.
//!
//! Uses `pulldown-cmark` for CommonMark parsing, then renders to Telegram's MarkdownV2 syntax
//! with proper escaping.
//!
//! # Escaping rules (Telegram MarkdownV2)
//! - Normal text: escape `_*[]()~`>#+-=|{}.!\`
//! - Inside code/pre: escape only `` ` `` and `\`
//! - Inside link URLs: escape only `)` and `\`
//!
//! # Conversion rules
//! - Headings → bold (`*text*`)
//! - Bold → `*text*`
//! - Italic → `_text_`
//! - Strikethrough → `~text~`
//! - Inline code → `` `text` ``
//! - Code block → ` ```lang\ntext\n``` `
//! - Link → `[text](url)`
//! - Blockquote → `>text` (each line prefixed)
//! - Table → rendered as ``` code block (monospace)
//! - HR → separator line
//! - List → bullet/numbered with escaped markers

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Characters that must be escaped in Telegram MarkdownV2 normal text.
const MDV2_ESCAPE: &[char] = &[
    '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!', '\\',
];

/// Escape text for Telegram MarkdownV2 normal body context.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if MDV2_ESCAPE.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape text inside code/pre blocks — only `` ` `` and `\` need escaping.
pub fn escape_code(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '`' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape text inside link URLs — only `)` and `\` need escaping.
pub fn escape_url(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == ')' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Convert standard Markdown to Telegram MarkdownV2 format.
///
/// This is the main entry point. It parses CommonMark (with tables and
/// strikethrough extensions) and produces a string suitable for Telegram's
/// `parse_mode: "MarkdownV2"`.
pub fn convert(md: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(md, opts);
    let mut renderer = Renderer::new();
    renderer.render(parser);
    renderer.finish()
}

struct Renderer {
    out: String,
    in_code_block: bool,
    in_blockquote: bool,
    // Table state
    in_table: bool,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    is_table_header: bool,
    // List state
    list_stack: Vec<Option<u64>>,
    // Link
    link_url: Option<String>,
    // Track if we need paragraph spacing
    last_was_block: bool,
}

impl Renderer {
    fn new() -> Self {
        Self {
            out: String::with_capacity(1024),
            in_code_block: false,
            in_blockquote: false,
            in_table: false,
            table_rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            is_table_header: false,
            list_stack: Vec::new(),
            link_url: None,
            last_was_block: false,
        }
    }

    fn render(&mut self, parser: Parser) {
        for event in parser {
            match event {
                Event::Start(tag) => self.start_tag(tag),
                Event::End(tag_end) => self.end_tag(tag_end),
                Event::Text(text) => self.text(&text),
                Event::Code(code) => self.inline_code(&code),
                Event::SoftBreak => self.soft_break(),
                Event::HardBreak => self.hard_break(),
                Event::Rule => self.rule(),
                _ => {}
            }
        }
    }

    fn finish(mut self) -> String {
        while self.out.ends_with('\n') || self.out.ends_with(' ') {
            self.out.pop();
        }
        self.out
    }

    fn push_text(&mut self, s: &str) {
        if self.in_table {
            self.current_cell.push_str(s);
        } else {
            self.out.push_str(s);
        }
    }

    fn start_tag(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { .. } => {
                self.ensure_newline();
                self.push_text("*");
            }
            Tag::Paragraph => {
                if self.last_was_block && !self.in_table {
                    self.ensure_newline();
                }
            }
            Tag::Strong => self.push_text("*"),
            Tag::Emphasis => self.push_text("_"),
            Tag::Strikethrough => self.push_text("~"),
            Tag::CodeBlock(kind) => {
                self.in_code_block = true;
                self.ensure_newline();
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let l = lang.trim().to_string();
                        if l.is_empty() { String::new() } else { l }
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.out.push_str("```");
                if !lang.is_empty() {
                    self.out.push_str(&lang);
                }
                self.out.push('\n');
            }
            Tag::Link { dest_url, .. } => {
                self.push_text("[");
                self.link_url = Some(dest_url.to_string());
            }
            Tag::Image { dest_url, .. } => {
                self.push_text("[");
                self.link_url = Some(dest_url.to_string());
            }
            Tag::List(start) => {
                self.list_stack.push(start);
            }
            Tag::Item => {
                self.ensure_newline();
                // Indent for nested lists
                let depth = self.list_stack.len();
                for _ in 1..depth {
                    self.push_text("  ");
                }
                let bullet = match self.list_stack.last_mut() {
                    Some(Some(n)) => {
                        let s = format!("{}\\. ", n);
                        *n += 1;
                        s
                    }
                    _ => "• ".to_string(),
                };
                self.push_text(&bullet);
            }
            Tag::BlockQuote(_) => {
                self.in_blockquote = true;
                self.ensure_newline();
                self.out.push('>');
            }
            Tag::Table(_) => {
                self.in_table = true;
                self.table_rows.clear();
            }
            Tag::TableHead => {
                self.is_table_header = true;
                self.current_row = Vec::new();
            }
            Tag::TableRow => {
                self.current_row = Vec::new();
            }
            Tag::TableCell => {
                self.current_cell.clear();
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag_end: TagEnd) {
        match tag_end {
            TagEnd::Heading(_) => {
                self.push_text("*");
                self.out.push('\n');
                self.last_was_block = true;
            }
            TagEnd::Paragraph => {
                if !self.in_table {
                    self.out.push('\n');
                    self.last_was_block = true;
                }
            }
            TagEnd::Strong => self.push_text("*"),
            TagEnd::Emphasis => self.push_text("_"),
            TagEnd::Strikethrough => self.push_text("~"),
            TagEnd::CodeBlock => {
                // Ensure code ends with newline before closing fence
                if !self.out.ends_with('\n') {
                    self.out.push('\n');
                }
                self.out.push_str("```\n");
                self.in_code_block = false;
                self.last_was_block = true;
            }
            TagEnd::Link => {
                let url = self.link_url.take().unwrap_or_default();
                let s = format!("]({})", escape_url(&url));
                self.push_text(&s);
            }
            TagEnd::Image => {
                let url = self.link_url.take().unwrap_or_default();
                let s = format!("]({})", escape_url(&url));
                self.push_text(&s);
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.last_was_block = true;
                }
            }
            TagEnd::Item => {
                if !self.out.ends_with('\n') {
                    self.out.push('\n');
                }
            }
            TagEnd::BlockQuote(_) => {
                self.in_blockquote = false;
                if !self.out.ends_with('\n') {
                    self.out.push('\n');
                }
                self.last_was_block = true;
            }
            TagEnd::Table => {
                self.in_table = false;
                self.render_table();
                self.last_was_block = true;
            }
            TagEnd::TableHead => {
                self.table_rows.push(self.current_row.clone());
                self.is_table_header = false;
            }
            TagEnd::TableRow => {
                if !self.is_table_header {
                    self.table_rows.push(self.current_row.clone());
                }
            }
            TagEnd::TableCell => {
                self.current_row.push(self.current_cell.clone());
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if self.in_code_block {
            // Inside code blocks, only escape ` and \
            self.out.push_str(&escape_code(text));
        } else if self.in_table {
            // Table cells: store raw text (will be inside a code block)
            self.current_cell.push_str(text);
        } else if self.in_blockquote {
            // Blockquote: escape text and prefix newlines with >
            let escaped = escape(text);
            for (i, line) in escaped.split('\n').enumerate() {
                if i > 0 {
                    self.out.push_str("\n>");
                }
                self.out.push_str(line);
            }
        } else {
            self.push_text(&escape(text));
        }
    }

    fn inline_code(&mut self, code: &str) {
        if self.in_table {
            self.current_cell.push_str(code);
        } else {
            self.out.push('`');
            self.out.push_str(&escape_code(code));
            self.out.push('`');
        }
    }

    fn soft_break(&mut self) {
        if self.in_table {
            self.current_cell.push(' ');
        } else if self.in_blockquote {
            self.out.push_str("\n>");
        } else {
            self.out.push('\n');
        }
    }

    fn hard_break(&mut self) {
        if self.in_table {
            self.current_cell.push(' ');
        } else if self.in_blockquote {
            self.out.push_str("\n>");
        } else {
            self.out.push('\n');
        }
    }

    fn rule(&mut self) {
        self.ensure_newline();
        self.out.push('\n');
        self.last_was_block = true;
    }

    fn ensure_newline(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
    }

    /// Render accumulated table as a monospace ``` code block.
    fn render_table(&mut self) {
        if self.table_rows.is_empty() {
            return;
        }
        let col_count = self.table_rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if col_count == 0 {
            return;
        }

        // Compute column widths
        let mut col_widths = vec![0usize; col_count];
        for row in &self.table_rows {
            for (ci, cell) in row.iter().enumerate() {
                col_widths[ci] = col_widths[ci].max(display_width(cell));
            }
        }

        self.ensure_newline();
        self.out.push_str("```\n");
        for (ri, row) in self.table_rows.iter().enumerate() {
            for (ci, w) in col_widths.iter().enumerate().take(col_count) {
                let cell = row.get(ci).map(|s| s.as_str()).unwrap_or("");
                let w = *w;
                let pad = w.saturating_sub(display_width(cell));
                self.out.push_str(cell);
                for _ in 0..pad {
                    self.out.push(' ');
                }
                if ci + 1 < col_count {
                    self.out.push_str(" │ ");
                }
            }
            self.out.push('\n');
            // Separator after header row
            if ri == 0 {
                for (ci, &w) in col_widths.iter().enumerate() {
                    for _ in 0..w {
                        self.out.push('─');
                    }
                    if ci + 1 < col_count {
                        self.out.push_str("─┼─");
                    }
                }
                self.out.push('\n');
            }
        }
        self.out.push_str("```\n");
    }
}

/// Approximate display width — CJK characters count as 2, others as 1.
fn display_width(s: &str) -> usize {
    s.chars().map(|c| if is_wide_char(c) { 2 } else { 1 }).sum()
}

fn is_wide_char(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{115F}'
        | '\u{2E80}'..='\u{A4CF}'
        | '\u{AC00}'..='\u{D7A3}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FE10}'..='\u{FE6F}'
        | '\u{FF01}'..='\u{FF60}'
        | '\u{FFE0}'..='\u{FFE6}'
        | '\u{20000}'..='\u{2FA1F}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_normal() {
        assert_eq!(escape("hello.world!"), "hello\\.world\\!");
        assert_eq!(escape("a*b_c"), "a\\*b\\_c");
    }

    #[test]
    fn test_escape_code() {
        assert_eq!(escape_code("a`b\\c"), "a\\`b\\\\c");
        assert_eq!(escape_code("normal text"), "normal text");
    }

    #[test]
    fn test_escape_url() {
        assert_eq!(
            escape_url("https://example.com/path(1)"),
            "https://example.com/path(1\\)"
        );
    }

    #[test]
    fn test_bold() {
        assert_eq!(convert("**bold**"), "*bold*");
    }

    #[test]
    fn test_italic() {
        assert_eq!(convert("*italic*"), "_italic_");
    }

    #[test]
    fn test_strikethrough() {
        assert_eq!(convert("~~deleted~~"), "~deleted~");
    }

    #[test]
    fn test_inline_code() {
        assert_eq!(convert("`code`"), "`code`");
        assert_eq!(convert("`a\\b`"), "`a\\\\b`");
    }

    #[test]
    fn test_code_block() {
        let input = "```rust\nfn main() {}\n```";
        let result = convert(input);
        assert!(result.starts_with("```rust\n"));
        assert!(result.contains("fn main() {}"));
        assert!(result.ends_with("\n```"));
    }

    #[test]
    fn test_code_block_no_lang() {
        let input = "```\nplain code\n```";
        let result = convert(input);
        assert!(result.starts_with("```\n"));
        assert!(result.contains("plain code"));
    }

    #[test]
    fn test_link() {
        let result = convert("[click](https://example.com)");
        assert_eq!(result, "[click](https://example.com)");
    }

    #[test]
    fn test_link_with_special_url() {
        let result = convert("[test](https://example.com/path(1))");
        assert!(result.contains("https://example.com/path(1\\)"));
    }

    #[test]
    fn test_unordered_list() {
        let result = convert("- item1\n- item2");
        assert!(result.contains("• item1"));
        assert!(result.contains("• item2"));
    }

    #[test]
    fn test_ordered_list() {
        let result = convert("1. first\n2. second");
        assert!(result.contains("1\\. first"));
        assert!(result.contains("2\\. second"));
    }

    #[test]
    fn test_heading() {
        let result = convert("# Title");
        assert!(result.contains("*Title*"));
    }

    #[test]
    fn test_special_chars_escaped() {
        let result = convert("hello.world!");
        assert_eq!(result, "hello\\.world\\!");
    }

    #[test]
    fn test_table() {
        let input = "| Name | Age |\n|------|-----|\n| Alice | 30 |";
        let result = convert(input);
        assert!(result.contains("```"));
        assert!(result.contains("Name"));
        assert!(result.contains("Alice"));
        assert!(result.contains("─"));
    }

    #[test]
    fn test_blockquote() {
        let result = convert("> hello world");
        assert!(result.contains(">"));
        assert!(result.contains("hello world"));
    }

    #[test]
    fn test_hr() {
        let result = convert("above\n\n---\n\nbelow");
        assert!(result.contains("above"));
        assert!(result.contains("below"));
        assert!(!result.contains("---"));
    }

    #[test]
    fn test_nested_formatting() {
        let result = convert("**bold _and italic_**");
        assert!(result.contains("*bold _and italic_*"));
    }

    #[test]
    fn test_complex_markdown() {
        let input = "# Hello\n\nThis is **bold** and *italic*.\n\n```python\nprint('hello')\n```\n\n- item 1\n- item 2";
        let result = convert(input);
        assert!(result.contains("*Hello*"));
        assert!(result.contains("*bold*"));
        assert!(result.contains("_italic_"));
        assert!(result.contains("```python"));
        assert!(result.contains("print('hello')"));
        assert!(result.contains("• item 1"));
    }
}
