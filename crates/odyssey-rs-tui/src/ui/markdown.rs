//! Markdown → ratatui `Line` renderer using pulldown-cmark.
//!
//! Call [`render_markdown`] to convert a markdown string into a `Vec<Line<'static>>`
//! suitable for use with a ratatui `Paragraph` widget.

use crate::ui::theme::Theme;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Convert `text` (assumed to be Markdown) into styled ratatui lines.
///
/// `base_style` is the default style applied to plain text spans.
pub fn render_markdown(text: &str, theme: &Theme, base_style: Style) -> Vec<Line<'static>> {
    let mut r = MdRenderer::new(theme, base_style);
    let opts = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION;
    for event in Parser::new_ext(text, opts) {
        r.handle(event);
    }
    r.finish()
}

// ── Renderer state ────────────────────────────────────────────────────────────

struct MdRenderer<'t> {
    theme: &'t Theme,
    base_style: Style,
    lines: Vec<Line<'static>>,
    /// Spans accumulated for the current logical line / paragraph.
    current_spans: Vec<Span<'static>>,
    /// Stack of inline styles (bold, italic, links, headings…).
    style_stack: Vec<Style>,
    /// Stack of list types: `None` = unordered, `Some(n)` = ordered starting at n.
    list_stack: Vec<Option<u64>>,
    /// Whether we are currently inside a fenced / indented code block.
    in_code_block: bool,
    /// Bullet / number prefix waiting to be prepended to the first span of a list item.
    pending_item_prefix: Option<String>,
}

impl<'t> MdRenderer<'t> {
    fn new(theme: &'t Theme, base_style: Style) -> Self {
        Self {
            theme,
            base_style,
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: vec![base_style],
            list_stack: Vec::new(),
            in_code_block: false,
            pending_item_prefix: None,
        }
    }

    fn cur_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or(self.base_style)
    }

    fn push_style(&mut self, s: Style) {
        self.style_stack.push(s);
    }

    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    /// Move accumulated spans into a new `Line`.  No-op when spans are empty.
    fn flush_line(&mut self) {
        if !self.current_spans.is_empty() {
            let spans = std::mem::take(&mut self.current_spans);
            self.lines.push(Line::from(spans));
        }
    }

    fn blank_line(&mut self) {
        self.lines.push(Line::from(Span::raw("")));
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.handle_start(tag),
            Event::End(tag) => self.handle_end(tag),
            Event::Rule => self.handle_rule(),
            Event::Code(code) => self.handle_inline_code(code.into_string()),
            Event::Text(text) => self.handle_text(text.into_string()),
            Event::SoftBreak => self.handle_soft_break(),
            Event::HardBreak => self.flush_line(),
            _ => {}
        }
    }

    fn handle_start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { level, .. } => self.handle_heading_start(level),
            Tag::Paragraph => self.apply_pending_item_prefix(),
            Tag::List(start) => {
                self.flush_line();
                self.list_stack.push(start);
            }
            Tag::Item => self.handle_item_start(),
            Tag::CodeBlock(kind) => self.handle_code_block_start(kind),
            Tag::BlockQuote(_) => self.handle_block_quote_start(),
            Tag::Strong => self.push_style(self.cur_style().add_modifier(Modifier::BOLD)),
            Tag::Emphasis => self.push_style(self.cur_style().add_modifier(Modifier::ITALIC)),
            Tag::Strikethrough => {
                self.push_style(self.cur_style().add_modifier(Modifier::CROSSED_OUT))
            }
            Tag::Link { .. } => self.push_style(
                self.cur_style()
                    .fg(self.theme.primary)
                    .add_modifier(Modifier::UNDERLINED),
            ),
            _ => {}
        }
    }

    fn handle_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush_line();
                self.blank_line();
            }
            TagEnd::Paragraph => {
                self.flush_line();
                if self.list_stack.is_empty() {
                    self.blank_line();
                }
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.blank_line();
                }
            }
            TagEnd::Item => {
                self.apply_pending_item_prefix();
                self.flush_line();
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                self.blank_line();
            }
            TagEnd::BlockQuote(_) => {
                self.pop_style();
                self.flush_line();
                self.blank_line();
            }
            TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link => {
                self.pop_style();
            }
            _ => {}
        }
    }

    fn handle_heading_start(&mut self, level: HeadingLevel) {
        self.flush_line();
        self.push_style(self.heading_style(level));
        let prefix = match level {
            HeadingLevel::H1 => "# ",
            HeadingLevel::H2 => "## ",
            HeadingLevel::H3 => "### ",
            _ => "#### ",
        };
        self.current_spans
            .push(Span::styled(prefix, self.cur_style()));
    }

    fn handle_item_start(&mut self) {
        self.flush_line();
        let depth = self.list_stack.len().saturating_sub(1);
        let indent = "  ".repeat(depth);
        let prefix = match self.list_stack.last_mut() {
            Some(Some(n)) => {
                let prefix = format!("{indent}{n}. ");
                *n += 1;
                prefix
            }
            _ => format!("{indent}• "),
        };
        self.pending_item_prefix = Some(prefix);
    }

    fn handle_code_block_start(&mut self, kind: CodeBlockKind<'_>) {
        self.flush_line();
        if let CodeBlockKind::Fenced(lang) = &kind
            && !lang.is_empty()
        {
            let lang_style = Style::default()
                .fg(self.theme.text_muted)
                .add_modifier(Modifier::ITALIC);
            self.lines
                .push(Line::from(Span::styled(format!(" {lang}"), lang_style)));
        }
        self.in_code_block = true;
    }

    fn handle_block_quote_start(&mut self) {
        self.flush_line();
        let style = Style::default()
            .fg(self.theme.text_muted)
            .add_modifier(Modifier::ITALIC);
        self.push_style(style);
        self.current_spans
            .push(Span::styled("│ ", self.cur_style()));
    }

    fn handle_rule(&mut self) {
        self.flush_line();
        self.lines.push(Line::from(Span::styled(
            "─".repeat(40),
            Style::default().fg(self.theme.border),
        )));
        self.blank_line();
    }

    fn handle_inline_code(&mut self, code: String) {
        self.apply_pending_item_prefix();
        let style = Style::default()
            .fg(self.theme.accent)
            .bg(self.theme.bg_popup);
        self.current_spans.push(Span::styled(code, style));
    }

    fn handle_text(&mut self, text: String) {
        if self.in_code_block {
            self.push_code_block_lines(&text);
            return;
        }

        self.apply_pending_item_prefix();
        let style = self.cur_style();
        self.current_spans.push(Span::styled(text, style));
    }

    fn push_code_block_lines(&mut self, text: &str) {
        let code_style = Style::default().fg(self.theme.accent);
        let trimmed = text.trim_end_matches('\n');
        for line in trimmed.lines() {
            self.lines
                .push(Line::from(Span::styled(line.to_owned(), code_style)));
        }
    }

    fn handle_soft_break(&mut self) {
        let style = self.cur_style();
        self.current_spans.push(Span::styled(" ", style));
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line();
        self.lines
    }

    /// If there is a pending list-item prefix, prepend it as the first span.
    fn apply_pending_item_prefix(&mut self) {
        if let Some(prefix) = self.pending_item_prefix.take() {
            let style = self.cur_style();
            self.current_spans.push(Span::styled(prefix, style));
        }
    }

    fn heading_style(&self, level: HeadingLevel) -> Style {
        let t = self.theme;
        match level {
            HeadingLevel::H1 => Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            HeadingLevel::H2 => Style::default()
                .fg(t.secondary)
                .add_modifier(Modifier::BOLD),
            HeadingLevel::H3 => Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            _ => Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render_markdown;
    use crate::ui::theme::ODYSSEY;
    use pretty_assertions::assert_eq;
    use ratatui::style::{Modifier, Style};

    fn line_texts(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn render_markdown_formats_headings_paragraphs_and_rules() {
        let lines = render_markdown("# Title\n\nParagraph\n\n---", &ODYSSEY, Style::default());
        let texts = line_texts(&lines);

        assert_eq!(
            texts,
            vec![
                "# Title".to_string(),
                "".to_string(),
                "Paragraph".to_string(),
                "".to_string(),
                "─".repeat(40),
                "".to_string(),
            ]
        );
        assert_eq!(lines[0].spans[0].style.fg, Some(ODYSSEY.primary));
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn render_markdown_formats_lists_and_block_quotes() {
        let lines = render_markdown(
            "- first\n- second\n\n1. one\n2. two\n\n> quoted",
            &ODYSSEY,
            Style::default(),
        );
        let texts = line_texts(&lines);

        assert_eq!(
            texts,
            vec![
                "• first".to_string(),
                "• second".to_string(),
                "".to_string(),
                "1. one".to_string(),
                "2. two".to_string(),
                "".to_string(),
                "│ quoted".to_string(),
                "".to_string(),
                "".to_string(),
            ]
        );
        assert_eq!(lines[6].spans[0].style.fg, Some(ODYSSEY.text_muted));
        assert!(
            lines[6].spans[0]
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
    }

    #[test]
    fn render_markdown_formats_inline_and_fenced_code() {
        let lines = render_markdown(
            "`inline`\n\n```rust\nlet x = 1;\n```\n",
            &ODYSSEY,
            Style::default(),
        );
        let texts = line_texts(&lines);

        assert_eq!(texts[0], "inline");
        assert_eq!(lines[0].spans[0].style.fg, Some(ODYSSEY.accent));
        assert_eq!(lines[0].spans[0].style.bg, Some(ODYSSEY.bg_popup));
        assert_eq!(texts[2], " rust");
        assert_eq!(lines[2].spans[0].style.fg, Some(ODYSSEY.text_muted));
        assert!(
            lines[2].spans[0]
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        assert_eq!(texts[3], "let x = 1;");
        assert_eq!(lines[3].spans[0].style.fg, Some(ODYSSEY.accent));
    }

    #[test]
    fn render_markdown_applies_inline_styles_and_soft_breaks() {
        let lines = render_markdown(
            "**bold** *italic* ~~old~~ [link](https://example.com)\nnext line",
            &ODYSSEY,
            Style::default(),
        );
        let texts = line_texts(&lines);

        assert_eq!(
            texts,
            vec!["bold italic old link next line".to_string(), "".to_string()]
        );
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            lines[0].spans[2]
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        assert!(
            lines[0].spans[4]
                .style
                .add_modifier
                .contains(Modifier::CROSSED_OUT)
        );
        assert_eq!(lines[0].spans[6].style.fg, Some(ODYSSEY.primary));
        assert!(
            lines[0].spans[6]
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
    }

    #[test]
    fn render_markdown_formats_nested_lists_with_indentation() {
        let lines = render_markdown(
            "1. parent\n   - child\n2. sibling",
            &ODYSSEY,
            Style::default(),
        );
        let texts = line_texts(&lines);

        assert_eq!(
            texts,
            vec![
                "1. parent".to_string(),
                "  • child".to_string(),
                "2. sibling".to_string(),
                "".to_string(),
            ]
        );
    }

    #[test]
    fn render_markdown_formats_lower_heading_levels_and_hard_breaks() {
        let lines = render_markdown(
            "## Section\n### Detail\n#### Note\n\nline one  \nline two",
            &ODYSSEY,
            Style::default(),
        );
        let texts = line_texts(&lines);

        assert_eq!(
            texts,
            vec![
                "## Section".to_string(),
                "".to_string(),
                "### Detail".to_string(),
                "".to_string(),
                "#### Note".to_string(),
                "".to_string(),
                "line one".to_string(),
                "line two".to_string(),
                "".to_string(),
            ]
        );
        assert_eq!(lines[0].spans[0].style.fg, Some(ODYSSEY.secondary));
        assert_eq!(lines[2].spans[0].style.fg, Some(ODYSSEY.accent));
        assert_eq!(lines[4].spans[0].style.fg, Some(ODYSSEY.text));
        assert!(
            lines[4].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }
}
