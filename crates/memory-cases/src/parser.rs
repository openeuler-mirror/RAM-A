use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentFormat {
    Markdown,
    Text,
    Unknown,
}

impl DocumentFormat {
    pub fn from_metadata(
        path: impl AsRef<Path>,
        mime_type: Option<&str>,
        file_name: Option<&str>,
    ) -> Self {
        if let Some(format) = mime_type.and_then(format_from_mime_type) {
            return format;
        }
        if let Some(format) = format_from_path(path.as_ref()) {
            return format;
        }
        if let Some(file_name) = file_name {
            if let Some(format) = format_from_path(Path::new(file_name)) {
                return format;
            }
        }
        DocumentFormat::Unknown
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseTopology {
    List,
}

impl ParseTopology {
    pub fn as_str(self) -> &'static str {
        match self {
            ParseTopology::List => "list",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseNodeType {
    Text,
    Code,
    Table,
}

impl ParseNodeType {
    pub fn as_str(self) -> &'static str {
        match self {
            ParseNodeType::Text => "text",
            ParseNodeType::Code => "code",
            ParseNodeType::Table => "table",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseNode {
    pub index: usize,
    pub content: String,
    pub node_type: ParseNodeType,
    pub level: usize,
    pub title: Option<String>,
    pub parent_index: Option<usize>,
    pub is_need_space: bool,
    pub is_need_newline: bool,
}

impl ParseNode {
    fn text(index: usize, content: impl Into<String>) -> Self {
        Self {
            index,
            content: content.into(),
            node_type: ParseNodeType::Text,
            level: 0,
            title: None,
            parent_index: None,
            is_need_space: false,
            is_need_newline: true,
        }
    }

    fn code(index: usize, content: impl Into<String>) -> Self {
        Self {
            index,
            content: content.into(),
            node_type: ParseNodeType::Code,
            level: 0,
            title: None,
            parent_index: None,
            is_need_space: false,
            is_need_newline: true,
        }
    }

    fn table(index: usize, content: impl Into<String>) -> Self {
        Self {
            index,
            content: content.into(),
            node_type: ParseNodeType::Table,
            level: 0,
            title: None,
            parent_index: None,
            is_need_space: false,
            is_need_newline: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseResult {
    pub format: DocumentFormat,
    pub topology: ParseTopology,
    pub nodes: Vec<ParseNode>,
}

pub struct ParserEngine;

impl ParserEngine {
    pub fn parse_file(
        path: impl AsRef<Path>,
        mime_type: Option<&str>,
        file_name: Option<&str>,
    ) -> io::Result<ParseResult> {
        let path = path.as_ref();
        let format = DocumentFormat::from_metadata(path, mime_type, file_name);
        Self::parse_file_as(path, format)
    }

    pub fn parse_file_as(
        path: impl AsRef<Path>,
        format: DocumentFormat,
    ) -> io::Result<ParseResult> {
        let path = path.as_ref();
        match format {
            DocumentFormat::Markdown => Self::parse_markdown_file(path),
            DocumentFormat::Text => Self::parse_text_file(path),
            DocumentFormat::Unknown => unsupported_format(path),
        }
    }

    pub fn parse_markdown_reader<R: BufRead>(reader: R) -> io::Result<ParseResult> {
        let mut parser = MarkdownNodeParser::default();
        for line in reader.lines() {
            parser.push_line(&line?);
        }
        Ok(ParseResult {
            format: DocumentFormat::Markdown,
            topology: ParseTopology::List,
            nodes: parser.finish(),
        })
    }

    pub fn parse_text_reader<R: BufRead>(reader: R) -> io::Result<ParseResult> {
        let mut content = String::new();
        for line in reader.lines() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(line?.trim_end());
        }

        let content = content.trim().to_string();
        let nodes = if content.is_empty() {
            Vec::new()
        } else {
            vec![ParseNode::text(0, content)]
        };
        Ok(ParseResult {
            format: DocumentFormat::Text,
            topology: ParseTopology::List,
            nodes,
        })
    }

    fn parse_markdown_file(path: &Path) -> io::Result<ParseResult> {
        let file = File::open(path)?;
        Self::parse_markdown_reader(BufReader::new(file))
    }

    fn parse_text_file(path: &Path) -> io::Result<ParseResult> {
        let file = File::open(path)?;
        Self::parse_text_reader(BufReader::new(file))
    }
}

#[derive(Default)]
struct MarkdownNodeParser {
    heading_path: Vec<(usize, String)>,
    paragraph_lines: Vec<String>,
    table_lines: Vec<String>,
    code_lines: Vec<String>,
    in_code_fence: bool,
    nodes: Vec<ParseNode>,
}

impl MarkdownNodeParser {
    fn push_line(&mut self, line: &str) {
        let trimmed = line.trim();

        if is_code_fence(trimmed) {
            if self.in_code_fence {
                self.flush_code();
                self.in_code_fence = false;
            } else {
                self.flush_paragraph();
                self.flush_table();
                self.in_code_fence = true;
            }
            return;
        }

        if self.in_code_fence {
            self.code_lines.push(line.to_string());
            return;
        }

        if let Some((level, title)) = markdown_heading(trimmed) {
            self.flush_paragraph();
            self.flush_table();
            self.heading_path.retain(|(existing, _)| *existing < level);
            self.heading_path.push((level, title));
            return;
        }

        if trimmed.is_empty() {
            self.flush_paragraph();
            self.flush_table();
            return;
        }

        if is_markdown_table_line(trimmed) {
            self.flush_paragraph();
            self.table_lines.push(trimmed.to_string());
            return;
        }

        self.flush_table();
        self.paragraph_lines.push(trimmed.to_string());
    }

    fn finish(mut self) -> Vec<ParseNode> {
        self.flush_paragraph();
        self.flush_table();
        if self.in_code_fence {
            self.flush_code();
        }
        self.nodes
    }

    fn flush_paragraph(&mut self) {
        if self.paragraph_lines.is_empty() {
            return;
        }
        let content = self.paragraph_lines.join("\n").trim().to_string();
        self.paragraph_lines.clear();
        if !content.is_empty() {
            let index = self.nodes.len();
            self.nodes.push(ParseNode::text(
                index,
                with_heading_path(&self.heading_path, &content),
            ));
            self.apply_heading_metadata(index);
        }
    }

    fn flush_table(&mut self) {
        if self.table_lines.is_empty() {
            return;
        }
        let lines = std::mem::take(&mut self.table_lines);
        for line in lines {
            if is_markdown_table_separator(&line) {
                continue;
            }
            let row = line
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .filter(|cell| !cell.is_empty())
                .collect::<Vec<_>>()
                .join(" | ");
            if !row.is_empty() {
                let index = self.nodes.len();
                self.nodes.push(ParseNode::table(
                    index,
                    with_heading_path(&self.heading_path, &row),
                ));
                self.apply_heading_metadata(index);
            }
        }
    }

    fn flush_code(&mut self) {
        if self.code_lines.is_empty() {
            return;
        }
        let content = self.code_lines.join("\n").trim().to_string();
        self.code_lines.clear();
        if !content.is_empty() {
            let index = self.nodes.len();
            self.nodes.push(ParseNode::code(
                index,
                with_heading_path(&self.heading_path, &content),
            ));
            self.apply_heading_metadata(index);
        }
    }

    fn apply_heading_metadata(&mut self, index: usize) {
        let Some((level, title)) = self.heading_path.last().cloned() else {
            return;
        };
        if let Some(node) = self.nodes.get_mut(index) {
            node.level = level;
            node.title = Some(title);
        }
    }
}

fn format_from_mime_type(mime_type: &str) -> Option<DocumentFormat> {
    let mime_type = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match mime_type.as_str() {
        "text/markdown" | "text/x-markdown" | "application/x-markdown" => {
            Some(DocumentFormat::Markdown)
        }
        "text/plain" => Some(DocumentFormat::Text),
        _ => None,
    }
}

fn format_from_path(path: &Path) -> Option<DocumentFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "md" | "markdown" | "mdx" => Some(DocumentFormat::Markdown),
        "txt" | "text" | "log" => Some(DocumentFormat::Text),
        _ => None,
    }
}

fn unsupported_format(path: &Path) -> io::Result<ParseResult> {
    let message = format!(
        "unsupported document format for parsing: path={}",
        path.display()
    );
    eprintln!("warning: {message}");
    Err(io::Error::new(io::ErrorKind::Unsupported, message))
}

fn markdown_heading(line: &str) -> Option<(usize, String)> {
    let hashes = line.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    if !line
        .chars()
        .nth(hashes)
        .map(char::is_whitespace)
        .unwrap_or(false)
    {
        return None;
    }
    let title = line[hashes..].trim();
    if title.is_empty() {
        None
    } else {
        Some((hashes, title.to_string()))
    }
}

fn is_code_fence(line: &str) -> bool {
    line.starts_with("```") || line.starts_with("~~~")
}

fn is_markdown_table_line(line: &str) -> bool {
    line.starts_with('|') && line.ends_with('|') && line.matches('|').count() >= 2
}

fn is_markdown_table_separator(line: &str) -> bool {
    let content = line.trim_matches('|').trim();
    !content.is_empty()
        && content
            .chars()
            .all(|ch| ch == '-' || ch == ':' || ch == '|' || ch.is_whitespace())
}

fn with_heading_path(path: &[(usize, String)], content: &str) -> String {
    if path.is_empty() {
        return content.trim().to_string();
    }
    let heading = path
        .iter()
        .map(|(_, title)| title.as_str())
        .collect::<Vec<_>>()
        .join(" > ");
    format!("{}\n\n{}", heading, content.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn detects_format_from_mime_before_extension() {
        let format = DocumentFormat::from_metadata(
            "release-notes.txt",
            Some("text/markdown; charset=utf-8"),
            None,
        );

        assert_eq!(format, DocumentFormat::Markdown);
    }

    #[test]
    fn ignores_unsupported_future_dispatch_formats() {
        let cases = [
            ("page.html", None),
            ("payload.json", None),
            ("manual.pdf", None),
            ("brief.docx", None),
            ("sheet.xlsx", None),
            ("slides.pptx", None),
            ("table.csv", None),
            (
                "upload.bin",
                Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            ),
        ];

        for (path, mime_type) in cases {
            assert_eq!(
                DocumentFormat::from_metadata(path, mime_type, None),
                DocumentFormat::Unknown
            );
        }
    }

    #[test]
    fn known_unimplemented_formats_return_unsupported() {
        let error = ParserEngine::parse_file_as("missing.pdf", DocumentFormat::Unknown)
            .expect_err("unknown parsing should be a placeholder");

        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn text_reader_returns_one_text_parse_node() {
        let result = ParserEngine::parse_text_reader(Cursor::new("alpha\nbeta")).unwrap();

        assert_eq!(result.format, DocumentFormat::Text);
        assert_eq!(result.topology, ParseTopology::List);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].node_type, ParseNodeType::Text);
        assert_eq!(result.nodes[0].content, "alpha\nbeta");
    }

    #[test]
    fn markdown_reader_emits_typed_parse_nodes() {
        let result = ParserEngine::parse_markdown_reader(Cursor::new(
            "# T\n\npara\n\n```rust\nfn main() {}\n```\n\n| key | value |\n| --- | --- |\n| a | b |",
        ))
        .unwrap();

        let node_types = result
            .nodes
            .iter()
            .map(|node| node.node_type)
            .collect::<Vec<_>>();

        assert_eq!(
            node_types,
            vec![
                ParseNodeType::Text,
                ParseNodeType::Code,
                ParseNodeType::Table,
                ParseNodeType::Table,
            ]
        );
        assert_eq!(result.nodes[0].content, "T\n\npara");
        assert_eq!(result.nodes[1].content, "T\n\nfn main() {}");
    }
}
