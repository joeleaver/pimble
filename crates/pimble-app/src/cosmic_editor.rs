//! Cosmic-text based rich text editor component
//!
//! This module provides a text editor built on cosmic-text that renders
//! to a pixel buffer for display in Slint.

use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight,
};
use std::sync::Mutex;
use std::time::Instant;

/// Type of list item
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ListType {
    Bullet,
    Numbered(u32),
    Task(bool), // bool = checked
}

/// Type of block element
#[derive(Debug, Clone, PartialEq)]
pub enum BlockType {
    Paragraph,
    Heading(u8),           // level 1-6
    CodeBlock(Option<String>), // optional language
    BlockQuote(u8),        // nesting level
    ThematicBreak,
    ListItem(ListType, u8), // list type, indent level
}

/// A parsed table with rows and columns
#[derive(Debug, Clone)]
pub struct ParsedTable {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub alignments: Vec<TableAlignment>,
    pub source_start_line: usize,
    pub source_end_line: usize,
}

/// Table column alignment
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TableAlignment {
    Left,
    Center,
    Right,
}

/// A styled text span for rich text rendering
#[derive(Debug, Clone)]
pub struct StyledSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strikethrough: bool,
    pub highlight: bool,            // ==highlighted text==
    pub heading_level: Option<u8>,  // 1-6 for h1-h6
    pub list_item: Option<ListType>,
    pub font_size: Option<f32>,     // Override font size (for headings)
    pub link_url: Option<String>,   // URL for links
    pub is_block_code: bool,        // Part of a code block (not inline code)
    pub block_quote_level: u8,      // Block quote nesting depth
    pub is_thematic_break: bool,    // Horizontal rule
    pub text_color: Option<Color>,  // Override text color (for links, code)
    pub background_color: Option<Color>, // Background highlight color
    pub table: Option<ParsedTable>, // Table data for table spans
}

/// Parse markdown text into styled spans
/// Handles block-level (headings, lists, code blocks, quotes) and inline formatting
pub fn parse_markdown_spans(text: &str) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    // Normalize line endings - handle both \r\n and \n
    let normalized_text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized_text.split('\n').collect();

    // Debug: print parsing info (only for non-trivial text)
    if text.len() > 10 {
        eprintln!("=== PARSE_MARKDOWN ===");
        eprintln!("Text length: {}, lines: {}", text.len(), lines.len());
        for (i, line) in lines.iter().take(5).enumerate() {
            let trimmed = line.trim_start();
            eprintln!("L{}: {:?}", i, if line.len() > 60 { &line[..60] } else { line });
            eprintln!("  bullet={} task={} heading={}",
                trimmed.starts_with("- ") || trimmed.starts_with("* "),
                trimmed.starts_with("- [ ]") || trimmed.starts_with("- [x]"),
                trimmed.starts_with("#"));
        }
        eprintln!("======================");
    }
    let mut numbered_list_counter = 0u32;
    let mut in_code_block = false;
    let mut code_block_lang: Option<String> = None;
    let mut code_block_content = String::new();
    let mut in_table = false;
    let mut table_lines: Vec<&str> = Vec::new();
    let mut table_start_line = 0usize;

    for (line_idx, line) in lines.iter().enumerate() {
        // Check for fenced code block start/end
        if line.starts_with("```") {
            if in_code_block {
                // End of code block - emit the collected content
                if !code_block_content.is_empty() {
                    // Remove trailing newline
                    if code_block_content.ends_with('\n') {
                        code_block_content.pop();
                    }
                    spans.push(StyledSpan {
                        text: code_block_content.clone(),
                        bold: false,
                        italic: false,
                        code: true,
                        strikethrough: false,
                        highlight: false,
                        heading_level: None,
                        list_item: None,
                        font_size: Some(13.0),
                        link_url: None,
                        is_block_code: true,
                        block_quote_level: 0,
                        is_thematic_break: false,
                        // GitHub dark code block colors
                        text_color: Some(Color::rgb(0xC9, 0xD1, 0xD9)), // #c9d1d9
                        background_color: Some(Color::rgba(0x16, 0x1B, 0x22, 0xFF)), // #161b22
                        table: None,
                    });
                }
                code_block_content.clear();
                in_code_block = false;
                code_block_lang = None;
            } else {
                // Start of code block - don't add newline (content follows immediately)
                in_code_block = true;
                let lang = line[3..].trim();
                code_block_lang = if lang.is_empty() { None } else { Some(lang.to_string()) };
            }
            // Only add newline for CLOSING fence to separate from following content
            // Opening fence produces no display output (code content follows directly)
            if !in_code_block && line_idx < lines.len() - 1 {
                spans.push(StyledSpan {
                    text: "\n".to_string(),
                    bold: false, italic: false, code: false, strikethrough: false, highlight: false,
                    heading_level: None, list_item: None, font_size: None,
                    link_url: None, is_block_code: false, block_quote_level: 0,
                    is_thematic_break: false, text_color: None, background_color: None, table: None,
                });
            }
            continue;
        }

        // Inside code block - collect content
        if in_code_block {
            code_block_content.push_str(line);
            code_block_content.push('\n');
            continue;
        }

        // Check for table rows (lines that contain | and have content)
        let trimmed = line.trim();
        let is_table_row = trimmed.contains('|') && !trimmed.is_empty();

        if is_table_row {
            if !in_table {
                // Starting a new table
                in_table = true;
                table_start_line = line_idx;
                table_lines.clear();
            }
            table_lines.push(trimmed);
            continue;
        } else if in_table {
            // End of table - process collected lines
            if let Some(table) = build_parsed_table(&table_lines, table_start_line, line_idx - 1) {
                // Create placeholder text with newlines to match table height
                // Each row (header + data rows) needs a line
                let num_rows = 1 + table.rows.len(); // header + data rows
                let placeholder_lines: String = (0..num_rows)
                    .map(|i| if i == 0 { format!("[Table {}x{}]", table.headers.len(), num_rows) } else { " ".to_string() })
                    .collect::<Vec<_>>()
                    .join("\n");

                spans.push(StyledSpan {
                    text: placeholder_lines,
                    bold: false, italic: false, code: false, strikethrough: false, highlight: false,
                    heading_level: None, list_item: None, font_size: Some(13.0),
                    link_url: None, is_block_code: false, block_quote_level: 0,
                    is_thematic_break: false, text_color: Some(Color::rgb(0x60, 0x80, 0xA0)),
                    background_color: None, table: Some(table),
                });
                // Add newline after table
                spans.push(StyledSpan {
                    text: "\n".to_string(),
                    bold: false, italic: false, code: false, strikethrough: false, highlight: false,
                    heading_level: None, list_item: None, font_size: None,
                    link_url: None, is_block_code: false, block_quote_level: 0,
                    is_thematic_break: false, text_color: None, background_color: None, table: None,
                });
            }
            in_table = false;
            table_lines.clear();
        }

        let mut line_content = *line;
        let mut heading_level: Option<u8> = None;
        let mut list_item: Option<ListType> = None;
        let mut font_size: Option<f32> = None;
        let mut block_quote_level: u8 = 0;
        let mut indent_level: u8 = 0;

        // Count leading spaces for indent level (for nested lists)
        let leading_spaces = line_content.len() - line_content.trim_start().len();
        indent_level = (leading_spaces / 2) as u8;
        line_content = line_content.trim_start();

        // Check for block quote (>)
        while line_content.starts_with('>') {
            block_quote_level += 1;
            line_content = line_content[1..].trim_start();
            tracing::debug!("Detected block quote level {}, remaining: {:?}", block_quote_level, line_content);
        }

        // Check for thematic break (---, ***, ___)
        let trimmed = line_content.trim();
        if (trimmed.starts_with("---") && trimmed.chars().all(|c| c == '-' || c.is_whitespace()))
            || (trimmed.starts_with("***") && trimmed.chars().all(|c| c == '*' || c.is_whitespace()))
            || (trimmed.starts_with("___") && trimmed.chars().all(|c| c == '_' || c.is_whitespace()))
        {
            if trimmed.chars().filter(|c| *c == '-' || *c == '*' || *c == '_').count() >= 3 {
                spans.push(StyledSpan {
                    text: "────────────────────────────────".to_string(),
                    bold: false, italic: false, code: false, strikethrough: false, highlight: false,
                    heading_level: None, list_item: None, font_size: Some(10.0),
                    link_url: None, is_block_code: false, block_quote_level,
                    is_thematic_break: true, text_color: Some(Color::rgb(0x60, 0x60, 0x60)), background_color: None, table: None,
                });
                if line_idx < lines.len() - 1 {
                    spans.push(StyledSpan {
                        text: "\n".to_string(),
                        bold: false, italic: false, code: false, strikethrough: false, highlight: false,
                        heading_level: None, list_item: None, font_size: None,
                        link_url: None, is_block_code: false, block_quote_level: 0,
                        is_thematic_break: false, text_color: None, background_color: None, table: None,
                    });
                }
                numbered_list_counter = 0;
                continue;
            }
        }

        // Check for heading (# at start of line)
        if line_content.starts_with('#') {
            let mut level = 0u8;
            for c in line_content.chars() {
                if c == '#' {
                    level += 1;
                } else {
                    break;
                }
            }
            if level > 0 && level <= 6 && (line_content.len() == level as usize || line_content.chars().nth(level as usize) == Some(' ')) {
                line_content = line_content[level as usize..].trim_start();
                heading_level = Some(level);
                // GitHub-like heading sizes (more subtle ratios)
                // Base font is 14px, so these are ~2x, 1.5x, 1.25x, 1.1x, 1x, 0.9x
                font_size = Some(match level {
                    1 => 24.0,
                    2 => 20.0,
                    3 => 17.0,
                    4 => 15.0,
                    5 => 14.0,
                    6 => 13.0,
                    _ => 14.0,
                });
                numbered_list_counter = 0;
            }
        }
        // Check for task list (- [ ] or - [x]) - before bullet list
        else if line_content.starts_with("- [ ] ") {
            tracing::debug!("Detected unchecked task: {:?}", line_content);
            line_content = &line_content[6..];
            list_item = Some(ListType::Task(false));
            numbered_list_counter = 0;
        }
        else if line_content.starts_with("- [x] ") || line_content.starts_with("- [X] ") {
            tracing::debug!("Detected checked task: {:?}", line_content);
            line_content = &line_content[6..];
            list_item = Some(ListType::Task(true));
            numbered_list_counter = 0;
        }
        // Check for bullet list (-, *, +)
        else if line_content.starts_with("- ") || line_content.starts_with("* ") || line_content.starts_with("+ ") {
            tracing::debug!("Detected bullet list: {:?}", line_content);
            line_content = &line_content[2..];
            list_item = Some(ListType::Bullet);
            numbered_list_counter = 0;
        }
        // Check for numbered list (1. 2. etc.)
        else if let Some(dot_pos) = line_content.find(". ") {
            let prefix = &line_content[..dot_pos];
            if prefix.chars().all(|c| c.is_ascii_digit()) && !prefix.is_empty() && dot_pos <= 9 {
                numbered_list_counter += 1;
                list_item = Some(ListType::Numbered(numbered_list_counter));
                line_content = &line_content[dot_pos + 2..];
            } else {
                numbered_list_counter = 0;
            }
        } else if !line_content.is_empty() {
            numbered_list_counter = 0;
        }

        // Parse inline formatting
        let line_spans = parse_inline_formatting(line_content, heading_level, list_item, font_size, block_quote_level, indent_level);
        spans.extend(line_spans);

        // Add newline
        if line_idx < lines.len() - 1 {
            spans.push(StyledSpan {
                text: "\n".to_string(),
                bold: false, italic: false, code: false, strikethrough: false, highlight: false,
                heading_level: None, list_item: None, font_size: None,
                link_url: None, is_block_code: false, block_quote_level: 0,
                is_thematic_break: false, text_color: None, background_color: None, table: None,
            });
        }
    }

    // Handle unclosed code block
    if in_code_block && !code_block_content.is_empty() {
        spans.push(StyledSpan {
            text: code_block_content,
            bold: false, italic: false, code: true, strikethrough: false, highlight: false,
            heading_level: None, list_item: None, font_size: Some(13.0),
            link_url: None, is_block_code: true, block_quote_level: 0,
            is_thematic_break: false,
            text_color: Some(Color::rgb(0xC9, 0xD1, 0xD9)), // #c9d1d9
            background_color: Some(Color::rgba(0x16, 0x1B, 0x22, 0xFF)), // #161b22
            table: None,
        });
    }

    // Handle table at end of document
    if in_table && !table_lines.is_empty() {
        if let Some(table) = build_parsed_table(&table_lines, table_start_line, lines.len() - 1) {
            let num_rows = 1 + table.rows.len();
            let placeholder_lines: String = (0..num_rows)
                .map(|i| if i == 0 { format!("[Table {}x{}]", table.headers.len(), num_rows) } else { " ".to_string() })
                .collect::<Vec<_>>()
                .join("\n");

            spans.push(StyledSpan {
                text: placeholder_lines,
                bold: false, italic: false, code: false, strikethrough: false, highlight: false,
                heading_level: None, list_item: None, font_size: Some(13.0),
                link_url: None, is_block_code: false, block_quote_level: 0,
                is_thematic_break: false, text_color: Some(Color::rgb(0x60, 0x80, 0xA0)),
                background_color: None, table: Some(table),
            });
        }
    }

    // If no spans, return empty span
    if spans.is_empty() {
        spans.push(StyledSpan {
            text: String::new(),
            bold: false, italic: false, code: false, strikethrough: false, highlight: false,
            heading_level: None, list_item: None, font_size: None,
            link_url: None, is_block_code: false, block_quote_level: 0,
            is_thematic_break: false, text_color: None, background_color: None, table: None,
        });
    }

    spans
}

/// Parse inline formatting within a line
fn parse_inline_formatting(
    text: &str,
    heading_level: Option<u8>,
    list_item: Option<ListType>,
    font_size: Option<f32>,
    block_quote_level: u8,
    indent_level: u8,
) -> Vec<StyledSpan> {
    let mut spans = Vec::new();

    // Add block quote prefix if needed - GitHub dark theme style
    if block_quote_level > 0 {
        let prefix = "│ ".repeat(block_quote_level as usize);
        spans.push(StyledSpan {
            text: prefix,
            bold: false, italic: false, code: false, strikethrough: false, highlight: false,
            heading_level: None, list_item: None, font_size: None,
            link_url: None, is_block_code: false, block_quote_level,
            // GitHub dark block quote border color
            is_thematic_break: false, text_color: Some(Color::rgb(0x3B, 0x43, 0x4B)), background_color: None, table: None,
        });
    }

    // Add indent for nested lists
    if indent_level > 0 && list_item.is_some() {
        let indent = "  ".repeat(indent_level as usize);
        spans.push(StyledSpan {
            text: indent,
            bold: false, italic: false, code: false, strikethrough: false, highlight: false,
            heading_level, list_item, font_size,
            link_url: None, is_block_code: false, block_quote_level,
            is_thematic_break: false, text_color: None, background_color: None, table: None,
        });
    }

    // Add list prefix if this is a list item (with base indent)
    if let Some(lt) = list_item {
        // Add base indent for all list items (GitHub-style left margin)
        let prefix = match lt {
            ListType::Bullet => "  • ".to_string(),      // 2-space indent + bullet
            ListType::Numbered(n) => format!("  {}. ", n), // 2-space indent + number
            ListType::Task(checked) => if checked { "  ☑ ".to_string() } else { "  ☐ ".to_string() },
        };
        spans.push(StyledSpan {
            text: prefix,
            bold: false, italic: false, code: false, strikethrough: false, highlight: false,
            heading_level, list_item: Some(lt), font_size,
            link_url: None, is_block_code: false, block_quote_level,
            is_thematic_break: false, text_color: None, background_color: None, table: None,
        });
    }

    let mut current_text = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut code = false;
    let mut strikethrough = false;
    let mut highlight = false;
    let mut link_url: Option<String> = None;

    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    // Helper to push current span
    let push_span = |spans: &mut Vec<StyledSpan>, text: String, bold, italic, code, strikethrough, highlight, link_url: Option<String>| {
        if text.is_empty() {
            return;
        }
        let text_color = if link_url.is_some() {
            Some(Color::rgb(0x58, 0xA6, 0xFF)) // #58a6ff - GitHub blue for links
        } else if code {
            Some(Color::rgb(0x79, 0xC0, 0xFF)) // #79c0ff - GitHub light blue for inline code
        } else if block_quote_level > 0 {
            Some(Color::rgb(0x8B, 0x94, 0x9E)) // #8b949e - GitHub dimmed text for block quotes
        } else {
            None
        };
        let background_color = if code {
            Some(Color::rgba(0x34, 0x39, 0x42, 0xFF)) // #343942 - inline code background
        } else if highlight {
            Some(Color::rgba(0xFF, 0xE0, 0x00, 0x60)) // Yellow highlight
        } else {
            None
        };
        spans.push(StyledSpan {
            text, bold, italic, code, strikethrough, highlight,
            heading_level, list_item, font_size,
            link_url, is_block_code: false, block_quote_level,
            is_thematic_break: false, text_color, background_color, table: None,
        });
    };

    while i < chars.len() {
        // Check for backslash escape
        if chars[i] == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            if "\\`*_{}[]()#+-.!~>".contains(next) {
                current_text.push(next);
                i += 2;
                continue;
            }
        }

        // Check for code (backtick)
        if chars[i] == '`' {
            if !current_text.is_empty() {
                push_span(&mut spans, current_text.clone(), bold, italic, code, strikethrough, highlight, link_url.clone());
                current_text.clear();
            }
            code = !code;
            i += 1;
            continue;
        }

        // Don't process other markers inside code
        if code {
            current_text.push(chars[i]);
            i += 1;
            continue;
        }

        // Check for angle bracket autolink <url> or <email>
        if chars[i] == '<' {
            if let Some((url, end_pos)) = parse_autolink(&chars, i) {
                if !current_text.is_empty() {
                    push_span(&mut spans, current_text.clone(), bold, italic, code, strikethrough, highlight, link_url.clone());
                    current_text.clear();
                }
                // Determine if it's an email or URL
                let display_url = url.clone();
                let full_url = if url.contains('@') && !url.contains("://") {
                    format!("mailto:{}", url)
                } else {
                    url
                };
                spans.push(StyledSpan {
                    text: display_url,
                    bold, italic, code: false, strikethrough, highlight,
                    heading_level, list_item, font_size,
                    link_url: Some(full_url), is_block_code: false, block_quote_level,
                    is_thematic_break: false, text_color: Some(Color::rgb(0x61, 0xAF, 0xEF)), background_color: None, table: None,
                });
                i = end_pos;
                continue;
            }
        }

        // Check for raw URL autolink (http:// or https://)
        if i + 7 < chars.len() && chars[i] == 'h' && chars[i + 1] == 't' && chars[i + 2] == 't' && chars[i + 3] == 'p' {
            if let Some((url, end_pos)) = parse_raw_url(&chars, i) {
                if !current_text.is_empty() {
                    push_span(&mut spans, current_text.clone(), bold, italic, code, strikethrough, highlight, link_url.clone());
                    current_text.clear();
                }
                spans.push(StyledSpan {
                    text: url.clone(),
                    bold, italic, code: false, strikethrough, highlight,
                    heading_level, list_item, font_size,
                    link_url: Some(url), is_block_code: false, block_quote_level,
                    is_thematic_break: false, text_color: Some(Color::rgb(0x61, 0xAF, 0xEF)), background_color: None, table: None,
                });
                i = end_pos;
                continue;
            }
        }

        // Check for image ![alt](url)
        if chars[i] == '!' && i + 1 < chars.len() && chars[i + 1] == '[' {
            if let Some((alt_text, url, end_pos)) = parse_link_or_image(&chars, i + 1) {
                if !current_text.is_empty() {
                    push_span(&mut spans, current_text.clone(), bold, italic, code, strikethrough, highlight, link_url.clone());
                    current_text.clear();
                }
                // Display as [Image: alt_text]
                let display = format!("🖼 {}", if alt_text.is_empty() { "image" } else { &alt_text });
                spans.push(StyledSpan {
                    text: display,
                    bold: false, italic: true, code: false, strikethrough: false, highlight: false,
                    heading_level, list_item, font_size,
                    link_url: Some(url), is_block_code: false, block_quote_level,
                    is_thematic_break: false, text_color: Some(Color::rgb(0x98, 0xC3, 0x79)), background_color: None, table: None,
                });
                i = end_pos;
                continue;
            }
        }

        // Check for link [text](url)
        if chars[i] == '[' {
            if let Some((link_text, url, end_pos)) = parse_link_or_image(&chars, i) {
                if !current_text.is_empty() {
                    push_span(&mut spans, current_text.clone(), bold, italic, code, strikethrough, highlight, link_url.clone());
                    current_text.clear();
                }
                spans.push(StyledSpan {
                    text: link_text,
                    bold, italic, code: false, strikethrough, highlight,
                    heading_level, list_item, font_size,
                    link_url: Some(url), is_block_code: false, block_quote_level,
                    is_thematic_break: false, text_color: Some(Color::rgb(0x61, 0xAF, 0xEF)), background_color: None, table: None,
                });
                i = end_pos;
                continue;
            }
        }

        // Check for strikethrough (~~)
        if i + 1 < chars.len() && chars[i] == '~' && chars[i + 1] == '~' {
            if !current_text.is_empty() {
                push_span(&mut spans, current_text.clone(), bold, italic, code, strikethrough, highlight, link_url.clone());
                current_text.clear();
            }
            strikethrough = !strikethrough;
            i += 2;
            continue;
        }

        // Check for highlight (==)
        if i + 1 < chars.len() && chars[i] == '=' && chars[i + 1] == '=' {
            if !current_text.is_empty() {
                push_span(&mut spans, current_text.clone(), bold, italic, code, strikethrough, highlight, link_url.clone());
                current_text.clear();
            }
            highlight = !highlight;
            i += 2;
            continue;
        }

        // Check for bold+italic (*** or ___) - must check before ** and *
        if i + 2 < chars.len() && ((chars[i] == '*' && chars[i + 1] == '*' && chars[i + 2] == '*')
            || (chars[i] == '_' && chars[i + 1] == '_' && chars[i + 2] == '_')) {
            if !current_text.is_empty() {
                push_span(&mut spans, current_text.clone(), bold, italic, code, strikethrough, highlight, link_url.clone());
                current_text.clear();
            }
            bold = !bold;
            italic = !italic;
            i += 3;
            continue;
        }

        // Check for bold (**) or (__) - must check before italic
        if i + 1 < chars.len() && ((chars[i] == '*' && chars[i + 1] == '*') || (chars[i] == '_' && chars[i + 1] == '_')) {
            if !current_text.is_empty() {
                push_span(&mut spans, current_text.clone(), bold, italic, code, strikethrough, highlight, link_url.clone());
                current_text.clear();
            }
            bold = !bold;
            i += 2;
            continue;
        }

        // Check for italic (*) or (_) - but not in middle of word for _
        if chars[i] == '*' || (chars[i] == '_' && (i == 0 || !chars[i-1].is_alphanumeric())) {
            if !current_text.is_empty() {
                push_span(&mut spans, current_text.clone(), bold, italic, code, strikethrough, highlight, link_url.clone());
                current_text.clear();
            }
            italic = !italic;
            i += 1;
            continue;
        }

        current_text.push(chars[i]);
        i += 1;
    }

    // Push remaining text
    if !current_text.is_empty() {
        push_span(&mut spans, current_text, bold, italic, code, strikethrough, highlight, link_url);
    }

    // Ensure at least one span
    if spans.is_empty() {
        spans.push(StyledSpan {
            text: String::new(),
            bold: false, italic: false, code: false, strikethrough: false, highlight: false,
            heading_level, list_item, font_size,
            link_url: None, is_block_code: false, block_quote_level,
            is_thematic_break: false, text_color: None, background_color: None, table: None,
        });
    }

    spans
}

/// Parse a link [text](url) or image starting at position i (which should be '[')
/// Returns (text, url, end_position) or None if not a valid link
fn parse_link_or_image(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    if start >= chars.len() || chars[start] != '[' {
        return None;
    }

    // Find the closing ]
    let mut bracket_depth = 1;
    let mut i = start + 1;
    let mut text = String::new();

    while i < chars.len() && bracket_depth > 0 {
        if chars[i] == '[' {
            bracket_depth += 1;
        } else if chars[i] == ']' {
            bracket_depth -= 1;
            if bracket_depth == 0 {
                break;
            }
        }
        if bracket_depth > 0 {
            text.push(chars[i]);
        }
        i += 1;
    }

    if bracket_depth != 0 || i >= chars.len() {
        return None;
    }

    // Should be followed by (url)
    i += 1; // Move past ]
    if i >= chars.len() || chars[i] != '(' {
        return None;
    }

    i += 1; // Move past (
    let mut url = String::new();
    let mut paren_depth = 1;

    while i < chars.len() && paren_depth > 0 {
        if chars[i] == '(' {
            paren_depth += 1;
            url.push(chars[i]);
        } else if chars[i] == ')' {
            paren_depth -= 1;
            if paren_depth > 0 {
                url.push(chars[i]);
            }
        } else {
            url.push(chars[i]);
        }
        i += 1;
    }

    if paren_depth != 0 {
        return None;
    }

    Some((text, url.trim().to_string(), i))
}

/// Parse an angle bracket autolink <url> or <email>
/// Returns (url_content, end_position) or None if not a valid autolink
fn parse_autolink(chars: &[char], start: usize) -> Option<(String, usize)> {
    if start >= chars.len() || chars[start] != '<' {
        return None;
    }

    let mut i = start + 1;
    let mut content = String::new();

    // Find the closing >
    while i < chars.len() {
        if chars[i] == '>' {
            // Valid autolink found
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Must be a URL (contains ://) or email (contains @)
            if trimmed.contains("://") || trimmed.contains('@') {
                return Some((trimmed.to_string(), i + 1));
            }
            return None;
        }
        if chars[i] == '<' || chars[i] == '\n' || chars[i].is_whitespace() {
            // Invalid character inside autolink
            return None;
        }
        content.push(chars[i]);
        i += 1;
    }

    None
}

/// Parse a raw URL starting with http:// or https://
/// Returns (url, end_position) or None if not a valid URL
fn parse_raw_url(chars: &[char], start: usize) -> Option<(String, usize)> {
    // Check for http:// or https://
    let mut i = start;
    let mut url = String::new();

    // Must start with http:// or https://
    let rest: String = chars[i..].iter().collect();
    if !rest.starts_with("http://") && !rest.starts_with("https://") {
        return None;
    }

    // Collect URL characters until we hit a terminator
    while i < chars.len() {
        let c = chars[i];
        // URL terminator characters (whitespace, certain punctuation at end)
        if c.is_whitespace() {
            break;
        }
        // Handle trailing punctuation that's not part of URL
        if c == ',' || c == '.' || c == '!' || c == '?' || c == ')' || c == ']' || c == ';' || c == ':' {
            // Look ahead to see if this is really the end
            let next = chars.get(i + 1);
            if next.is_none() || next.unwrap().is_whitespace() || *next.unwrap() == '\n' {
                break;
            }
        }
        url.push(c);
        i += 1;
    }

    // Minimum valid URL: http://x (8 chars) or https://x (9 chars)
    if url.len() >= 8 && (url.starts_with("http://") || url.starts_with("https://")) {
        Some((url, i))
    } else {
        None
    }
}

/// Check if a line is a table separator row (contains only |, -, :, and whitespace)
fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('-') {
        return false;
    }
    // Must have at least one |
    if !trimmed.contains('|') {
        return false;
    }
    // All characters must be |, -, :, or whitespace
    trimmed.chars().all(|c| c == '|' || c == '-' || c == ':' || c.is_whitespace())
}

/// Parse a table row into cells
fn parse_table_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    // Remove leading and trailing |
    let content = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let content = content.strip_suffix('|').unwrap_or(content);

    // Split by | and collect cells
    content.split('|').map(|s| s.trim().to_string()).collect()
}

/// Parse alignment from separator cell (e.g., :---, :---:, ---:)
fn parse_alignment(cell: &str) -> TableAlignment {
    let trimmed = cell.trim();
    let starts_colon = trimmed.starts_with(':');
    let ends_colon = trimmed.ends_with(':');

    match (starts_colon, ends_colon) {
        (true, true) => TableAlignment::Center,
        (false, true) => TableAlignment::Right,
        _ => TableAlignment::Left,
    }
}

/// Build a ParsedTable from collected table lines
fn build_parsed_table(lines: &[&str], start_line: usize, end_line: usize) -> Option<ParsedTable> {
    if lines.len() < 2 {
        return None; // Need at least header and separator
    }

    // Find the separator row (should be second line)
    let separator_idx = lines.iter().position(|l| is_table_separator(l))?;
    if separator_idx == 0 {
        return None; // Separator can't be first line
    }

    // Parse header (first row before separator)
    let headers = parse_table_row(lines[0]);
    if headers.is_empty() {
        return None;
    }

    // Parse alignments from separator
    let separator_cells = parse_table_row(lines[separator_idx]);
    let alignments: Vec<TableAlignment> = separator_cells.iter()
        .map(|c| parse_alignment(c))
        .collect();

    // Parse data rows (all rows after separator)
    let rows: Vec<Vec<String>> = lines[separator_idx + 1..]
        .iter()
        .filter(|l| !is_table_separator(l))
        .map(|l| parse_table_row(l))
        .collect();

    Some(ParsedTable {
        headers,
        rows,
        alignments,
        source_start_line: start_line,
        source_end_line: end_line,
    })
}

/// RGBA pixel buffer for rendering
pub struct PixelBuffer {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA format, 4 bytes per pixel
}

impl PixelBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width * height * 4) as usize;
        Self {
            width,
            height,
            pixels: vec![0; size],
        }
    }

    pub fn clear(&mut self, color: Color) {
        for chunk in self.pixels.chunks_exact_mut(4) {
            chunk[0] = color.r();
            chunk[1] = color.g();
            chunk[2] = color.b();
            chunk[3] = color.a();
        }
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x < self.width && y < self.height {
            let idx = ((y * self.width + x) * 4) as usize;
            // Alpha blend
            let alpha = color.a() as f32 / 255.0;
            let inv_alpha = 1.0 - alpha;

            self.pixels[idx] = (color.r() as f32 * alpha + self.pixels[idx] as f32 * inv_alpha) as u8;
            self.pixels[idx + 1] = (color.g() as f32 * alpha + self.pixels[idx + 1] as f32 * inv_alpha) as u8;
            self.pixels[idx + 2] = (color.b() as f32 * alpha + self.pixels[idx + 2] as f32 * inv_alpha) as u8;
            self.pixels[idx + 3] = 255; // Full alpha for destination
        }
    }

    /// Fill a rectangle with a color
    pub fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: Color) {
        for dy in 0..h {
            for dx in 0..w {
                let px = x + dx as i32;
                let py = y + dy as i32;
                if px >= 0 && py >= 0 {
                    self.set_pixel(px as u32, py as u32, color);
                }
            }
        }
    }
}

/// Configuration for the cosmic text editor
pub struct EditorConfig {
    pub font_size: f32,
    pub line_height: f32,
    pub font_family: Family<'static>,
    pub text_color: Color,
    pub background_color: Color,
    pub selection_color: Color,
    pub cursor_color: Color,
    pub padding: f32,  // Padding around content
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            line_height: 22.0,  // Slightly more line height for readability
            font_family: Family::SansSerif,
            // GitHub dark theme colors
            text_color: Color::rgb(0xC9, 0xD1, 0xD9),      // #c9d1d9 - main text
            background_color: Color::rgb(0x0D, 0x11, 0x17), // #0d1117 - dark background
            selection_color: Color::rgba(0x26, 0x4F, 0x78, 0x99), // #264f78 - selection blue
            cursor_color: Color::rgb(0x58, 0xA6, 0xFF),    // #58a6ff - bright blue cursor
            padding: 16.0,  // 16px padding on all sides
        }
    }
}

/// Get the source marker length and display prefix for a line
/// Returns (source_marker_bytes_to_skip, display_prefix_string)
fn get_line_marker_info(line: &str) -> (usize, String) {
    let trimmed_line = line.trim_start();
    let leading_spaces = line.len() - trimmed_line.len();
    let indent_prefix = if leading_spaces > 0 {
        "  ".repeat(leading_spaces / 2)
    } else {
        String::new()
    };

    // Check for fenced code block markers (```)
    if trimmed_line.starts_with("```") {
        // The entire line is the fence - skip it all, display nothing
        return (line.len(), String::new());
    }

    // Check for thematic break (---, ***, ___)
    let trimmed = trimmed_line.trim();
    if (trimmed.starts_with("---") && trimmed.chars().all(|c| c == '-' || c.is_whitespace()))
        || (trimmed.starts_with("***") && trimmed.chars().all(|c| c == '*' || c.is_whitespace()))
        || (trimmed.starts_with("___") && trimmed.chars().all(|c| c == '_' || c.is_whitespace()))
    {
        if trimmed.chars().filter(|c| *c == '-' || *c == '*' || *c == '_').count() >= 3 {
            // Skip all, display the horizontal line
            return (line.len(), "────────────────────────────────".to_string());
        }
    }

    // Check for block quote (>)
    let mut quote_level = 0;
    let mut content_start = leading_spaces;
    let mut temp = trimmed_line;
    while temp.starts_with('>') {
        quote_level += 1;
        temp = temp[1..].trim_start();
        content_start = line.len() - temp.len();
    }
    if quote_level > 0 {
        let quote_prefix = "│ ".repeat(quote_level);
        return (content_start, format!("{}{}", indent_prefix, quote_prefix));
    }

    // Check for heading (# at start of line)
    if trimmed_line.starts_with('#') {
        let mut level = 0usize;
        for c in trimmed_line.chars() {
            if c == '#' {
                level += 1;
            } else {
                break;
            }
        }
        if level > 0 && level <= 6 && (trimmed_line.len() == level || trimmed_line.chars().nth(level) == Some(' ')) {
            let after_hashes = &trimmed_line[level..];
            let marker_len = leading_spaces + level + if after_hashes.starts_with(' ') { 1 } else { 0 };
            return (marker_len, indent_prefix);
        }
    }

    // Check for task list (- [ ] or - [x])
    // Note: display prefix must match what parse_inline_formatting produces (2-space base indent)
    if trimmed_line.starts_with("- [ ] ") {
        return (leading_spaces + 6, format!("{}  ☐ ", indent_prefix));
    }
    if trimmed_line.starts_with("- [x] ") || trimmed_line.starts_with("- [X] ") {
        return (leading_spaces + 6, format!("{}  ☑ ", indent_prefix));
    }

    // Check for bullet list (-, *, +)
    // Note: display prefix must match what parse_inline_formatting produces (2-space base indent)
    if trimmed_line.starts_with("- ") || trimmed_line.starts_with("* ") || trimmed_line.starts_with("+ ") {
        return (leading_spaces + 2, format!("{}  ★ ", indent_prefix));
    }

    // Check for numbered list (1. 2. etc.)
    // Note: display prefix must match what parse_inline_formatting produces (2-space base indent)
    if let Some(dot_pos) = trimmed_line.find(". ") {
        let prefix = &trimmed_line[..dot_pos];
        if prefix.chars().all(|c| c.is_ascii_digit()) && !prefix.is_empty() && dot_pos <= 9 {
            return (leading_spaces + dot_pos + 2, format!("{}  {}. ", indent_prefix, prefix));
        }
    }

    // No block-level marker
    (0, String::new())
}

/// Convert inline source position to display position (handles **, *, `, ~~, == markers)
fn source_to_display_inline(content: &str, source_pos: usize) -> usize {
    let mut display_pos = 0;
    let mut source_idx = 0;
    let chars: Vec<char> = content.chars().collect();
    let mut in_code = false;

    while source_idx < chars.len() && source_idx < source_pos {
        let c = chars[source_idx];

        // Check for code backtick
        if c == '`' {
            in_code = !in_code;
            source_idx += 1;
            continue;
        }

        if !in_code {
            // Check for ~~ (strikethrough)
            if source_idx + 1 < chars.len() && c == '~' && chars[source_idx + 1] == '~' {
                source_idx += 2;
                continue;
            }

            // Check for == (highlight)
            if source_idx + 1 < chars.len() && c == '=' && chars[source_idx + 1] == '=' {
                source_idx += 2;
                continue;
            }

            // Check for *** (bold+italic)
            if source_idx + 2 < chars.len() && c == '*' && chars[source_idx + 1] == '*' && chars[source_idx + 2] == '*' {
                source_idx += 3;
                continue;
            }

            // Check for ** (bold)
            if source_idx + 1 < chars.len() && c == '*' && chars[source_idx + 1] == '*' {
                source_idx += 2;
                continue;
            }

            // Check for * or _ (italic)
            if c == '*' || c == '_' {
                source_idx += 1;
                continue;
            }
        }

        // Regular character - count it
        display_pos += c.len_utf8();
        source_idx += 1;
    }

    display_pos
}

/// Convert inline display position to source position (handles **, *, `, ~~, == markers)
fn display_to_source_inline(content: &str, display_pos: usize) -> usize {
    let mut current_display_pos = 0;
    let mut source_idx = 0;
    let chars: Vec<char> = content.chars().collect();
    let mut in_code = false;

    while source_idx < chars.len() && current_display_pos < display_pos {
        let c = chars[source_idx];

        // Check for code backtick
        if c == '`' {
            in_code = !in_code;
            source_idx += 1;
            continue;
        }

        if !in_code {
            // Check for ~~ (strikethrough)
            if source_idx + 1 < chars.len() && c == '~' && chars[source_idx + 1] == '~' {
                source_idx += 2;
                continue;
            }

            // Check for == (highlight)
            if source_idx + 1 < chars.len() && c == '=' && chars[source_idx + 1] == '=' {
                source_idx += 2;
                continue;
            }

            // Check for *** (bold+italic)
            if source_idx + 2 < chars.len() && c == '*' && chars[source_idx + 1] == '*' && chars[source_idx + 2] == '*' {
                source_idx += 3;
                continue;
            }

            // Check for ** (bold)
            if source_idx + 1 < chars.len() && c == '*' && chars[source_idx + 1] == '*' {
                source_idx += 2;
                continue;
            }

            // Check for * or _ (italic)
            if c == '*' || c == '_' {
                source_idx += 1;
                continue;
            }
        }

        // Regular character
        current_display_pos += c.len_utf8();
        source_idx += 1;
    }

    // Return the byte position in the source
    chars[..source_idx].iter().map(|c| c.len_utf8()).sum()
}

/// Selected table cell information
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TableCellSelection {
    pub table_index: usize,  // Index of the table span in the document
    pub row: usize,          // Row index (0 = header)
    pub col: usize,          // Column index
    pub cursor_in_cell: usize, // Cursor position within cell text
}

/// A cosmic-text based editor that manages Buffer directly
pub struct SimpleCosmicEditor {
    config: EditorConfig,
    width: f32,
    height: f32,
    needs_redraw: bool,
    cached_buffer: Option<PixelBuffer>,
    // Text content
    text: String,
    // Cursor position (byte offset)
    cursor: usize,
    // Selection anchor (byte offset), None if no selection
    selection_anchor: Option<usize>,
    // Scroll position (y offset in pixels)
    scroll_y: f32,
    // Zoom level (1.0 = 100%)
    zoom: f32,
    // Cached content height for scroll calculations
    cached_content_height: f32,
    // Cursor blinking state
    cursor_visible: bool,
    last_blink_toggle: Instant,
    // Table cell editing state
    selected_table_cell: Option<TableCellSelection>,
}

impl SimpleCosmicEditor {
    pub fn new(config: EditorConfig) -> Self {
        Self {
            config,
            width: 400.0,
            height: 300.0,
            needs_redraw: true,
            cached_buffer: None,
            text: String::new(),
            cursor: 0,
            selection_anchor: None,
            scroll_y: 0.0,
            zoom: 1.0,
            cached_content_height: 0.0,
            cursor_visible: true,
            last_blink_toggle: Instant::now(),
            selected_table_cell: None,
        }
    }

    /// Update cursor blink state. Returns true if the cursor visibility changed.
    /// Should be called periodically (e.g., every 100ms) to update blink state.
    pub fn update_blink(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_blink_toggle);
        if elapsed.as_millis() >= 500 {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink_toggle = now;
            self.needs_redraw = true;
            true
        } else {
            false
        }
    }

    /// Reset cursor blink to visible (called on cursor movement/typing)
    pub fn reset_blink(&mut self) {
        self.cursor_visible = true;
        self.last_blink_toggle = Instant::now();
    }

    pub fn set_scroll(&mut self, scroll_y: f32) {
        if (self.scroll_y - scroll_y).abs() > 0.5 {
            self.scroll_y = scroll_y;
            self.needs_redraw = true;
        }
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        if (self.zoom - zoom).abs() > 0.01 {
            self.zoom = zoom.clamp(0.5, 3.0);
            self.needs_redraw = true;
        }
    }

    pub fn content_height(&self) -> f32 {
        self.cached_content_height
    }

    pub fn set_size(&mut self, width: f32, height: f32) {
        if (self.width - width).abs() > 0.1 || (self.height - height).abs() > 0.1 {
            self.width = width;
            self.height = height;
            self.needs_redraw = true;
            self.cached_buffer = None;
        }
    }

    pub fn set_text(&mut self, text: &str) {
        // Normalize line endings to \n for consistent position mapping
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.text != normalized {
            // Debug: log when text is set (only for non-trivial content)
            static SET_TEXT_DEBUG: std::sync::Once = std::sync::Once::new();
            if normalized.len() > 10 {
                SET_TEXT_DEBUG.call_once(|| {
                    eprintln!("=== SET_TEXT CALLED ===");
                    eprintln!("Text length: {} (normalized from {})", normalized.len(), text.len());
                    let preview = if normalized.len() > 100 { &normalized[..100] } else { &normalized };
                    eprintln!("Preview: {:?}", preview);
                    eprintln!("========================");
                });
            }
            self.text = normalized;
            self.cursor = self.cursor.min(self.text.len());
            self.selection_anchor = None;
            self.needs_redraw = true;
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor_position(&self) -> usize {
        self.cursor
    }

    /// Handle a character insertion
    pub fn insert_char(&mut self, c: char) {
        // Delete selection first if any
        self.delete_selection();

        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Handle backspace
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            self.reset_blink();
            return;
        }

        if self.cursor > 0 {
            // Find previous char boundary
            let prev = self.text[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
            self.needs_redraw = true;
            self.reset_blink();
        }
    }

    /// Handle delete key
    pub fn delete(&mut self) {
        if self.delete_selection() {
            self.reset_blink();
            return;
        }

        if self.cursor < self.text.len() {
            // Find next char boundary
            let next = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
            self.text.drain(self.cursor..next);
            self.needs_redraw = true;
            self.reset_blink();
        }
    }

    /// Handle enter key
    pub fn enter(&mut self) {
        self.delete_selection();
        self.text.insert(self.cursor, '\n');
        self.cursor += 1;
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Move cursor left
    pub fn move_left(&mut self, extend_selection: bool) {
        if extend_selection && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        } else if !extend_selection {
            self.selection_anchor = None;
        }

        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.needs_redraw = true;
            self.reset_blink();
        }
    }

    /// Move cursor right
    pub fn move_right(&mut self, extend_selection: bool) {
        if extend_selection && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        } else if !extend_selection {
            self.selection_anchor = None;
        }

        if self.cursor < self.text.len() {
            self.cursor = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
            self.needs_redraw = true;
            self.reset_blink();
        }
    }

    /// Move cursor to start of line
    pub fn move_home(&mut self, extend_selection: bool) {
        if extend_selection && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        } else if !extend_selection {
            self.selection_anchor = None;
        }

        // Find start of current line
        let line_start = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.cursor = line_start;
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Move cursor to end of line
    pub fn move_end(&mut self, extend_selection: bool) {
        if extend_selection && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        } else if !extend_selection {
            self.selection_anchor = None;
        }

        // Find end of current line
        let line_end = self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len());
        self.cursor = line_end;
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Move cursor up one line
    pub fn move_up(&mut self, extend_selection: bool) {
        if extend_selection && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        } else if !extend_selection {
            self.selection_anchor = None;
        }

        // Find current line start and column
        let line_start = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let column = self.cursor - line_start;

        // If we're on the first line, go to start
        if line_start == 0 {
            self.cursor = 0;
            self.needs_redraw = true;
            self.reset_blink();
            return;
        }

        // Find previous line start
        let prev_line_end = line_start - 1; // Position of the \n
        let prev_line_start = self.text[..prev_line_end]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prev_line_len = prev_line_end - prev_line_start;

        // Move to same column on previous line (or end if shorter)
        self.cursor = prev_line_start + column.min(prev_line_len);
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Move cursor down one line
    pub fn move_down(&mut self, extend_selection: bool) {
        if extend_selection && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        } else if !extend_selection {
            self.selection_anchor = None;
        }

        // Find current line start and column
        let line_start = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let column = self.cursor - line_start;

        // Find next line start
        let next_line_start = match self.text[self.cursor..].find('\n') {
            Some(i) => self.cursor + i + 1,
            None => {
                // Already on last line, go to end
                self.cursor = self.text.len();
                self.needs_redraw = true;
                self.reset_blink();
                return;
            }
        };

        // Find next line end
        let next_line_end = self.text[next_line_start..]
            .find('\n')
            .map(|i| next_line_start + i)
            .unwrap_or(self.text.len());
        let next_line_len = next_line_end - next_line_start;

        // Move to same column on next line (or end if shorter)
        self.cursor = next_line_start + column.min(next_line_len);
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Delete selection, returns true if there was a selection
    fn delete_selection(&mut self) -> bool {
        if let Some(anchor) = self.selection_anchor.take() {
            let start = anchor.min(self.cursor);
            let end = anchor.max(self.cursor);
            self.text.drain(start..end);
            self.cursor = start;
            self.needs_redraw = true;
            true
        } else {
            false
        }
    }

    /// Get selection range (start, end) if any
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.selection_anchor.map(|anchor| {
            let start = anchor.min(self.cursor);
            let end = anchor.max(self.cursor);
            (start, end)
        })
    }

    /// Get the selected text, if any
    pub fn get_selected_text(&self) -> Option<String> {
        self.selection().map(|(start, end)| {
            self.text[start..end].to_string()
        })
    }

    /// Paste text at cursor position (replaces selection if any)
    pub fn paste(&mut self, text: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Select all text
    pub fn select_all(&mut self) {
        self.selection_anchor = Some(0);
        self.cursor = self.text.len();
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Handle click at pixel position
    pub fn click(&mut self, x: f32, y: f32, font_system: &mut FontSystem) {
        // First check if click is within a table cell
        if let Some(cell_selection) = self.find_table_cell_at(x, y, font_system) {
            self.selected_table_cell = Some(cell_selection);
            self.selection_anchor = None;
            self.needs_redraw = true;
            self.reset_blink();
            return;
        }

        // Not in a table - clear table selection and handle normal click
        self.selected_table_cell = None;
        self.selection_anchor = None;
        self.cursor = self.position_from_point(x, y, font_system);
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Get the currently selected table cell
    pub fn selected_table_cell(&self) -> Option<TableCellSelection> {
        self.selected_table_cell
    }

    /// Clear table cell selection
    pub fn clear_table_selection(&mut self) {
        if self.selected_table_cell.is_some() {
            self.selected_table_cell = None;
            self.needs_redraw = true;
        }
    }

    /// Get the text content of the currently selected cell
    pub fn get_selected_cell_text(&self) -> Option<String> {
        let selection = self.selected_table_cell?;
        let spans = parse_markdown_spans(&self.text);
        let span = spans.get(selection.table_index)?;
        let table = span.table.as_ref()?;

        if selection.row == 0 {
            table.headers.get(selection.col).cloned()
        } else {
            table.rows.get(selection.row - 1)
                .and_then(|row| row.get(selection.col))
                .cloned()
        }
    }

    /// Get the number of columns in the selected table
    pub fn get_selected_table_cols(&self) -> Option<usize> {
        let selection = self.selected_table_cell?;
        let spans = parse_markdown_spans(&self.text);
        let span = spans.get(selection.table_index)?;
        let table = span.table.as_ref()?;
        Some(table.headers.len())
    }

    /// Get the number of rows in the selected table (including header)
    pub fn get_selected_table_rows(&self) -> Option<usize> {
        let selection = self.selected_table_cell?;
        let spans = parse_markdown_spans(&self.text);
        let span = spans.get(selection.table_index)?;
        let table = span.table.as_ref()?;
        Some(1 + table.rows.len())
    }

    /// Update a cell's content in the source markdown text
    fn update_cell_in_source(&mut self, table_index: usize, row: usize, col: usize, new_text: &str) {
        let spans = parse_markdown_spans(&self.text);
        let Some(span) = spans.get(table_index) else { return };
        let Some(table) = span.table.as_ref() else { return };

        let lines: Vec<&str> = self.text.lines().collect();
        let start_line = table.source_start_line;
        let end_line = table.source_end_line;

        // Determine which source line to edit
        // row 0 = header (source line 0)
        // row 1+ = data rows (source line 2+, skipping separator at line 1)
        let source_row_idx = if row == 0 {
            start_line
        } else {
            start_line + 1 + row // +1 for separator line
        };

        if source_row_idx > end_line || source_row_idx >= lines.len() {
            return;
        }

        // Parse the row and replace the cell
        let row_line = lines[source_row_idx];
        let cells: Vec<&str> = row_line
            .trim()
            .trim_start_matches('|')
            .trim_end_matches('|')
            .split('|')
            .map(|s| s.trim())
            .collect();

        if col >= cells.len() {
            return;
        }

        // Rebuild the row with the new cell content
        let mut new_cells: Vec<String> = cells.iter().map(|s| s.to_string()).collect();
        new_cells[col] = new_text.to_string();
        let new_row = format!("| {} |", new_cells.join(" | "));

        // Rebuild the entire text with the modified row
        let mut new_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        new_lines[source_row_idx] = new_row;
        self.text = new_lines.join("\n");
        self.needs_redraw = true;
    }

    /// Insert a character at the cursor position within the selected cell
    pub fn insert_char_in_cell(&mut self, c: char) {
        let Some(selection) = self.selected_table_cell else { return };

        if let Some(mut cell_text) = self.get_selected_cell_text() {
            // Clamp cursor position
            let cursor_pos = selection.cursor_in_cell.min(cell_text.len());

            // Insert the character
            cell_text.insert(cursor_pos, c);

            // Update source
            self.update_cell_in_source(selection.table_index, selection.row, selection.col, &cell_text);

            // Move cursor forward
            if let Some(ref mut sel) = self.selected_table_cell {
                sel.cursor_in_cell = cursor_pos + c.len_utf8();
            }

            self.reset_blink();
        }
    }

    /// Handle backspace in a table cell
    pub fn backspace_in_cell(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };

        if let Some(mut cell_text) = self.get_selected_cell_text() {
            let cursor_pos = selection.cursor_in_cell.min(cell_text.len());

            if cursor_pos > 0 {
                // Find previous char boundary
                let prev = cell_text[..cursor_pos]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);

                cell_text.drain(prev..cursor_pos);

                // Update source
                self.update_cell_in_source(selection.table_index, selection.row, selection.col, &cell_text);

                // Move cursor back
                if let Some(ref mut sel) = self.selected_table_cell {
                    sel.cursor_in_cell = prev;
                }

                self.reset_blink();
            }
        }
    }

    /// Handle delete key in a table cell
    pub fn delete_in_cell(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };

        if let Some(mut cell_text) = self.get_selected_cell_text() {
            let cursor_pos = selection.cursor_in_cell.min(cell_text.len());

            if cursor_pos < cell_text.len() {
                // Find next char boundary
                let next = cell_text[cursor_pos..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| cursor_pos + i)
                    .unwrap_or(cell_text.len());

                cell_text.drain(cursor_pos..next);

                // Update source
                self.update_cell_in_source(selection.table_index, selection.row, selection.col, &cell_text);
                self.reset_blink();
            }
        }
    }

    /// Move cursor left within a table cell
    pub fn move_cell_cursor_left(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        if selection.cursor_in_cell == 0 { return; }

        let Some(cell_text) = self.get_selected_cell_text() else { return };
        let cursor_pos = selection.cursor_in_cell.min(cell_text.len());
        let new_cursor = cell_text[..cursor_pos]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);

        if let Some(ref mut sel) = self.selected_table_cell {
            sel.cursor_in_cell = new_cursor;
        }
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Move cursor right within a table cell
    pub fn move_cell_cursor_right(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let Some(cell_text) = self.get_selected_cell_text() else { return };

        let cursor_pos = selection.cursor_in_cell.min(cell_text.len());
        if cursor_pos >= cell_text.len() { return; }

        let new_cursor = cell_text[cursor_pos..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| cursor_pos + i)
            .unwrap_or(cell_text.len());

        if let Some(ref mut sel) = self.selected_table_cell {
            sel.cursor_in_cell = new_cursor;
        }
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Move to the next cell (Tab navigation)
    pub fn move_to_next_cell(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let Some(num_cols) = self.get_selected_table_cols() else { return };
        let Some(num_rows) = self.get_selected_table_rows() else { return };

        let mut new_col = selection.col + 1;
        let mut new_row = selection.row;

        if new_col >= num_cols {
            new_col = 0;
            new_row += 1;
            if new_row >= num_rows {
                // Clear selection when tabbing past last cell
                self.selected_table_cell = None;
                self.needs_redraw = true;
                return;
            }
        }

        self.selected_table_cell = Some(TableCellSelection {
            table_index: selection.table_index,
            row: new_row,
            col: new_col,
            cursor_in_cell: 0,
        });
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Move to the previous cell (Shift+Tab navigation)
    pub fn move_to_prev_cell(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let Some(num_cols) = self.get_selected_table_cols() else { return };

        let mut new_col = selection.col;
        let mut new_row = selection.row;

        if new_col == 0 {
            if new_row == 0 {
                // Clear selection when shift-tabbing before first cell
                self.selected_table_cell = None;
                self.needs_redraw = true;
                return;
            }
            new_row -= 1;
            new_col = num_cols - 1;
        } else {
            new_col -= 1;
        }

        self.selected_table_cell = Some(TableCellSelection {
            table_index: selection.table_index,
            row: new_row,
            col: new_col,
            cursor_in_cell: 0,
        });
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Move to the cell above (Up arrow)
    pub fn move_to_cell_above(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };

        if selection.row > 0 {
            self.selected_table_cell = Some(TableCellSelection {
                table_index: selection.table_index,
                row: selection.row - 1,
                col: selection.col,
                cursor_in_cell: 0,
            });
            self.needs_redraw = true;
            self.reset_blink();
        } else {
            // Clear selection when moving up from first row
            self.selected_table_cell = None;
            self.needs_redraw = true;
        }
    }

    /// Move to the cell below (Down arrow)
    pub fn move_to_cell_below(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let Some(num_rows) = self.get_selected_table_rows() else { return };

        if selection.row + 1 < num_rows {
            self.selected_table_cell = Some(TableCellSelection {
                table_index: selection.table_index,
                row: selection.row + 1,
                col: selection.col,
                cursor_in_cell: 0,
            });
            self.needs_redraw = true;
            self.reset_blink();
        } else {
            // Clear selection when moving down from last row
            self.selected_table_cell = None;
            self.needs_redraw = true;
        }
    }

    /// Move to the cell to the left (Left arrow at cell start)
    pub fn move_to_cell_left(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };

        // Only move to previous cell if cursor is at the start
        if selection.cursor_in_cell == 0 && selection.col > 0 {
            self.selected_table_cell = Some(TableCellSelection {
                table_index: selection.table_index,
                row: selection.row,
                col: selection.col - 1,
                cursor_in_cell: 0, // Move to start of previous cell
            });
            // Set cursor to end of the cell
            if let Some(cell_text) = self.get_selected_cell_text() {
                if let Some(ref mut sel) = self.selected_table_cell {
                    sel.cursor_in_cell = cell_text.len();
                }
            }
            self.needs_redraw = true;
            self.reset_blink();
        }
    }

    /// Move to the cell to the right (Right arrow at cell end)
    pub fn move_to_cell_right(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let Some(num_cols) = self.get_selected_table_cols() else { return };
        let cell_len = self.get_selected_cell_text().map(|s| s.len()).unwrap_or(0);

        // Only move to next cell if cursor is at the end
        if selection.cursor_in_cell >= cell_len && selection.col + 1 < num_cols {
            self.selected_table_cell = Some(TableCellSelection {
                table_index: selection.table_index,
                row: selection.row,
                col: selection.col + 1,
                cursor_in_cell: 0, // Move to start of next cell
            });
            self.needs_redraw = true;
            self.reset_blink();
        }
    }

    /// Check if a table cell is currently selected
    pub fn has_table_cell_selected(&self) -> bool {
        self.selected_table_cell.is_some()
    }

    /// Get the position for the table toolbar (above the selected table)
    /// Returns (x, y) position in viewport coordinates, or None if no table cell is selected
    pub fn get_table_toolbar_position(&self, font_system: &mut FontSystem) -> Option<(f32, f32)> {
        let selection = self.selected_table_cell?;
        let spans = parse_markdown_spans(&self.text);
        let span = spans.get(selection.table_index)?;
        let table = span.table.as_ref()?;

        // Build a cosmic-text buffer to get line positions (must match render() setup exactly)
        let metrics = Metrics::new(self.config.font_size * self.zoom, self.config.line_height * self.zoom);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(self.width), Some(self.height));

        let rich_spans: Vec<(&str, Attrs)> = spans.iter().map(|span| {
            // Apply zoom to font sizes (must match render() exactly)
            let font_size = span.font_size.unwrap_or(self.config.font_size) * self.zoom;
            let line_height = font_size * (self.config.line_height / self.config.font_size);
            let mut attrs = Attrs::new()
                .family(self.config.font_family)
                .metrics(Metrics::new(font_size, line_height));

            // Must include all styling that affects layout (matching render())
            if span.bold || span.heading_level.is_some() {
                attrs = attrs.weight(Weight::BOLD);
            }
            if span.italic {
                attrs = attrs.style(Style::Italic);
            }
            if span.code || span.is_block_code {
                attrs = attrs.family(Family::Monospace);
            }

            (span.text.as_str(), attrs)
        }).collect();

        buffer.set_rich_text(font_system, rich_spans, Attrs::new(), Shaping::Advanced);
        buffer.set_size(font_system, Some(self.width), None);
        buffer.set_scroll(cosmic_text::Scroll::default());
        buffer.shape_until_scroll(font_system, true);

        let zoomed_font_size = self.config.font_size * self.zoom;
        let cell_padding = 4.0 * self.zoom;

        for (span_idx, span) in spans.iter().enumerate() {
            if span_idx == selection.table_index {
                if let Some(ref _table) = span.table {
                    // Find the y position of this table by searching for its placeholder text in the buffer
                    let placeholder_start = &span.text[..span.text.find('\n').unwrap_or(span.text.len())];
                    let mut table_y = 0.0f32;
                    let mut found = false;

                    // Search through buffer lines for the table placeholder
                    for run in buffer.layout_runs() {
                        if let Some(line) = buffer.lines.get(run.line_i) {
                            let line_text = line.text();
                            if line_text.contains(placeholder_start) || line_text.starts_with("[Table") {
                                table_y = run.line_y - zoomed_font_size;
                                found = true;
                                break;
                            }
                        }
                    }

                    if !found {
                        return None;
                    }

                    // Calculate column widths to get table width
                    let num_cols = table.headers.len();
                    let mut col_widths: Vec<f32> = vec![80.0 * self.zoom; num_cols];

                    for (i, header) in table.headers.iter().enumerate() {
                        let text_width = self.measure_text_width(header, font_system, true);
                        if i < col_widths.len() {
                            col_widths[i] = col_widths[i].max(text_width + cell_padding * 2.0);
                        }
                    }
                    for row in &table.rows {
                        for (i, cell) in row.iter().enumerate() {
                            let text_width = self.measure_text_width(cell, font_system, false);
                            if i < col_widths.len() {
                                col_widths[i] = col_widths[i].max(text_width + cell_padding * 2.0);
                            }
                        }
                    }

                    let table_width: f32 = col_widths.iter().sum();
                    let toolbar_width = 200.0; // Width of the toolbar

                    // Position toolbar centered above the table (or left-aligned if table is narrow)
                    let toolbar_x = if table_width > toolbar_width {
                        (table_width - toolbar_width) / 2.0
                    } else {
                        0.0
                    };

                    // Position 40px above the table (in viewport coordinates, accounting for scroll)
                    let toolbar_y = (table_y - self.scroll_y - 40.0).max(0.0);

                    return Some((toolbar_x, toolbar_y));
                }
            }
        }

        None
    }

    /// Add a new row below the current cell
    pub fn add_row_below(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let spans = parse_markdown_spans(&self.text);
        let Some(span) = spans.get(selection.table_index) else { return };
        let Some(table) = span.table.as_ref() else { return };

        let num_cols = table.headers.len();
        let lines: Vec<&str> = self.text.lines().collect();
        let start_line = table.source_start_line;
        let end_line = table.source_end_line;

        // Determine which source line to insert after
        // row 0 = header, row 1+ = data rows
        let insert_after_line = if selection.row == 0 {
            start_line + 1 // After separator line
        } else {
            start_line + 1 + selection.row // After the current data row
        };

        if insert_after_line > lines.len() {
            return;
        }

        // Create a new empty row
        let empty_cells: Vec<&str> = vec![""; num_cols];
        let new_row = format!("| {} |", empty_cells.join(" | "));

        // Rebuild the text with the new row inserted
        let mut new_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        new_lines.insert(insert_after_line + 1, new_row);
        self.text = new_lines.join("\n");

        // Move selection to the new row
        self.selected_table_cell = Some(TableCellSelection {
            table_index: selection.table_index,
            row: selection.row + 1,
            col: selection.col,
            cursor_in_cell: 0,
        });

        self.needs_redraw = true;
    }

    /// Add a new row above the current cell
    pub fn add_row_above(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let spans = parse_markdown_spans(&self.text);
        let Some(span) = spans.get(selection.table_index) else { return };
        let Some(table) = span.table.as_ref() else { return };

        // Can't add a row above the header
        if selection.row == 0 {
            return;
        }

        let num_cols = table.headers.len();
        let lines: Vec<&str> = self.text.lines().collect();
        let start_line = table.source_start_line;

        // Determine which source line to insert before
        let insert_before_line = start_line + 1 + selection.row; // +1 for separator, then row index

        if insert_before_line > lines.len() {
            return;
        }

        // Create a new empty row
        let empty_cells: Vec<&str> = vec![""; num_cols];
        let new_row = format!("| {} |", empty_cells.join(" | "));

        // Rebuild the text with the new row inserted
        let mut new_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        new_lines.insert(insert_before_line, new_row);
        self.text = new_lines.join("\n");

        // Keep selection on the same cell (which is now one row lower in terms of data)
        // The row stays the same visually since we inserted above
        self.needs_redraw = true;
    }

    /// Add a new column to the right of the current cell
    pub fn add_column_right(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let spans = parse_markdown_spans(&self.text);
        let Some(span) = spans.get(selection.table_index) else { return };
        let Some(table) = span.table.as_ref() else { return };

        let lines: Vec<&str> = self.text.lines().collect();
        let start_line = table.source_start_line;
        let end_line = table.source_end_line;

        let mut new_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();

        // Process each table line
        for line_idx in start_line..=end_line {
            if line_idx >= new_lines.len() { break; }

            let line = &new_lines[line_idx];
            let is_separator = line.contains("---") || line.contains(":--") || line.contains("--:");

            let cells: Vec<&str> = line
                .trim()
                .trim_start_matches('|')
                .trim_end_matches('|')
                .split('|')
                .map(|s| s.trim())
                .collect();

            let mut new_cells: Vec<String> = cells.iter().map(|s| s.to_string()).collect();

            // Insert empty cell or separator after the current column
            let insert_idx = (selection.col + 1).min(new_cells.len());
            if is_separator {
                new_cells.insert(insert_idx, "---".to_string());
            } else {
                new_cells.insert(insert_idx, String::new());
            }

            new_lines[line_idx] = format!("| {} |", new_cells.join(" | "));
        }

        self.text = new_lines.join("\n");
        self.needs_redraw = true;
    }

    /// Add a new column to the left of the current cell
    pub fn add_column_left(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let spans = parse_markdown_spans(&self.text);
        let Some(span) = spans.get(selection.table_index) else { return };
        let Some(table) = span.table.as_ref() else { return };

        let lines: Vec<&str> = self.text.lines().collect();
        let start_line = table.source_start_line;
        let end_line = table.source_end_line;

        let mut new_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();

        // Process each table line
        for line_idx in start_line..=end_line {
            if line_idx >= new_lines.len() { break; }

            let line = &new_lines[line_idx];
            let is_separator = line.contains("---") || line.contains(":--") || line.contains("--:");

            let cells: Vec<&str> = line
                .trim()
                .trim_start_matches('|')
                .trim_end_matches('|')
                .split('|')
                .map(|s| s.trim())
                .collect();

            let mut new_cells: Vec<String> = cells.iter().map(|s| s.to_string()).collect();

            // Insert empty cell or separator before the current column
            let insert_idx = selection.col.min(new_cells.len());
            if is_separator {
                new_cells.insert(insert_idx, "---".to_string());
            } else {
                new_cells.insert(insert_idx, String::new());
            }

            new_lines[line_idx] = format!("| {} |", new_cells.join(" | "));
        }

        self.text = new_lines.join("\n");

        // Move selection to the right to stay on the same logical cell
        self.selected_table_cell = Some(TableCellSelection {
            table_index: selection.table_index,
            row: selection.row,
            col: selection.col + 1,
            cursor_in_cell: 0,
        });

        self.needs_redraw = true;
    }

    /// Delete the current row (cannot delete header row)
    pub fn delete_row(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let spans = parse_markdown_spans(&self.text);
        let Some(span) = spans.get(selection.table_index) else { return };
        let Some(table) = span.table.as_ref() else { return };

        // Can't delete header row
        if selection.row == 0 {
            return;
        }

        // Need at least one data row to delete
        if table.rows.len() <= 1 {
            return;
        }

        let lines: Vec<&str> = self.text.lines().collect();
        let start_line = table.source_start_line;

        // Determine which source line to delete
        let delete_line = start_line + 1 + selection.row; // +1 for separator

        if delete_line >= lines.len() {
            return;
        }

        let mut new_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        new_lines.remove(delete_line);
        self.text = new_lines.join("\n");

        // Move selection up if we deleted the last row
        let new_row = selection.row.min(table.rows.len() - 1);
        self.selected_table_cell = Some(TableCellSelection {
            table_index: selection.table_index,
            row: new_row,
            col: selection.col,
            cursor_in_cell: 0,
        });

        self.needs_redraw = true;
    }

    /// Delete the current column (must keep at least one column)
    pub fn delete_column(&mut self) {
        let Some(selection) = self.selected_table_cell else { return };
        let spans = parse_markdown_spans(&self.text);
        let Some(span) = spans.get(selection.table_index) else { return };
        let Some(table) = span.table.as_ref() else { return };

        // Need at least 2 columns to delete one
        if table.headers.len() <= 1 {
            return;
        }

        let lines: Vec<&str> = self.text.lines().collect();
        let start_line = table.source_start_line;
        let end_line = table.source_end_line;

        let mut new_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();

        // Process each table line
        for line_idx in start_line..=end_line {
            if line_idx >= new_lines.len() { break; }

            let line = &new_lines[line_idx];
            let cells: Vec<&str> = line
                .trim()
                .trim_start_matches('|')
                .trim_end_matches('|')
                .split('|')
                .map(|s| s.trim())
                .collect();

            let mut new_cells: Vec<String> = cells.iter().map(|s| s.to_string()).collect();

            if selection.col < new_cells.len() {
                new_cells.remove(selection.col);
            }

            new_lines[line_idx] = format!("| {} |", new_cells.join(" | "));
        }

        self.text = new_lines.join("\n");

        // Move selection left if we deleted the rightmost column
        let new_col = selection.col.min(table.headers.len() - 2);
        self.selected_table_cell = Some(TableCellSelection {
            table_index: selection.table_index,
            row: selection.row,
            col: new_col,
            cursor_in_cell: 0,
        });

        self.needs_redraw = true;
    }

    /// Find which table cell (if any) is at the given pixel position
    fn find_table_cell_at(&self, x: f32, y: f32, font_system: &mut FontSystem) -> Option<TableCellSelection> {
        let spans = parse_markdown_spans(&self.text);
        let padding = self.config.padding;  // Padding doesn't scale with zoom

        // Account for padding and scroll - convert viewport coordinates to content coordinates
        let content_x = (x - padding).max(0.0);
        let actual_y = (y - padding).max(0.0) + self.scroll_y;

        // Build a cosmic-text buffer to get line positions (must match render() setup exactly)
        let metrics = Metrics::new(self.config.font_size * self.zoom, self.config.line_height * self.zoom);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(self.width), Some(self.height));

        let rich_spans: Vec<(&str, Attrs)> = spans.iter().map(|span| {
            // Apply zoom to font sizes (must match render() exactly)
            let font_size = span.font_size.unwrap_or(self.config.font_size) * self.zoom;
            let line_height = font_size * (self.config.line_height / self.config.font_size);
            let mut attrs = Attrs::new()
                .family(self.config.font_family)
                .metrics(Metrics::new(font_size, line_height));

            // Must include all styling that affects layout (matching render())
            if span.bold || span.heading_level.is_some() {
                attrs = attrs.weight(Weight::BOLD);
            }
            if span.italic {
                attrs = attrs.style(Style::Italic);
            }
            if span.code || span.is_block_code {
                attrs = attrs.family(Family::Monospace);
            }

            (span.text.as_str(), attrs)
        }).collect();

        buffer.set_rich_text(font_system, rich_spans, Attrs::new(), Shaping::Advanced);
        buffer.set_size(font_system, Some(self.width), None);
        buffer.set_scroll(cosmic_text::Scroll::default());
        buffer.shape_until_scroll(font_system, true);

        let zoomed_line_height = self.config.line_height * self.zoom;
        let zoomed_font_size = self.config.font_size * self.zoom;
        let cell_padding = 4.0 * self.zoom;

        for (span_idx, span) in spans.iter().enumerate() {
            if let Some(ref table) = span.table {
                // Find the y position of this table by searching for its placeholder text in the buffer
                // The placeholder is "[Table NxM]" at the start of the span
                let placeholder_start = &span.text[..span.text.find('\n').unwrap_or(span.text.len())];
                let mut table_y = 0.0f32;
                let mut found = false;

                // Search through buffer lines for the table placeholder
                for run in buffer.layout_runs() {
                    if let Some(line) = buffer.lines.get(run.line_i) {
                        let line_text = line.text();
                        if line_text.contains(placeholder_start) || line_text.starts_with("[Table") {
                            table_y = run.line_y - zoomed_font_size;
                            found = true;
                            break;
                        }
                    }
                }

                if !found {
                    continue;
                }

                // Calculate column widths
                let num_cols = table.headers.len();
                let mut col_widths: Vec<f32> = vec![80.0 * self.zoom; num_cols];

                for (i, header) in table.headers.iter().enumerate() {
                    let text_width = self.measure_text_width(header, font_system, true);
                    if i < col_widths.len() {
                        col_widths[i] = col_widths[i].max(text_width + cell_padding * 2.0);
                    }
                }
                for row in &table.rows {
                    for (i, cell) in row.iter().enumerate() {
                        let text_width = self.measure_text_width(cell, font_system, false);
                        if i < col_widths.len() {
                            col_widths[i] = col_widths[i].max(text_width + cell_padding * 2.0);
                        }
                    }
                }

                let total_width: f32 = col_widths.iter().sum();
                let row_height = zoomed_line_height;
                let total_rows = 1 + table.rows.len();
                let total_height = row_height * total_rows as f32;

                // Debug logging for table hit detection (table is at content_x=0)
                let table_x = 0.0f32;
                let y_in_bounds = actual_y >= table_y && actual_y < table_y + total_height;
                let x_in_bounds = content_x >= table_x && content_x < table_x + total_width;
                tracing::info!(
                    "Table hit test: click=({}, {}), content_x={}, scroll_y={}, actual_y={}, table_y={}, table_h={}, total_width={}, y_in_bounds={}, x_in_bounds={}",
                    x, y, content_x, self.scroll_y, actual_y, table_y, total_height, total_width, y_in_bounds, x_in_bounds
                );

                // Check if click is within this table
                if y_in_bounds && x_in_bounds {
                    tracing::info!("Table bounds check PASSED for span_idx={}, total_rows={}, num_cols={}", span_idx, total_rows, num_cols);
                    // Find which row
                    let row_idx = ((actual_y - table_y) / row_height) as usize;
                    let row_idx = row_idx.min(total_rows - 1);
                    tracing::info!("Calculated row_idx={}", row_idx);

                    // Find which column (using content_x which has padding subtracted)
                    let mut col_idx = 0;
                    let mut col_x = table_x;
                    for (i, &width) in col_widths.iter().enumerate() {
                        if content_x >= col_x && content_x < col_x + width {
                            col_idx = i;
                            break;
                        }
                        col_x += width;
                    }
                    tracing::info!("Calculated col_idx={}", col_idx);

                    // Calculate cursor position within cell based on content_x position
                    let cell_start_x = col_widths[..col_idx].iter().sum::<f32>();
                    let x_in_cell = (content_x - cell_start_x - cell_padding).max(0.0);

                    // Get cell text and estimate cursor position
                    let cell_text = if row_idx == 0 {
                        table.headers.get(col_idx).map(|s| s.as_str()).unwrap_or("")
                    } else {
                        table.rows.get(row_idx - 1)
                            .and_then(|row| row.get(col_idx))
                            .map(|s| s.as_str())
                            .unwrap_or("")
                    };
                    tracing::info!("Got cell_text, len={}", cell_text.len());

                    // Estimate character position based on average character width
                    let avg_char_width = if !cell_text.is_empty() {
                        let text_width = self.measure_text_width(cell_text, font_system, row_idx == 0);
                        text_width / cell_text.len() as f32
                    } else {
                        zoomed_font_size * 0.5
                    };

                    let cursor_in_cell = ((x_in_cell / avg_char_width) as usize).min(cell_text.len());

                    tracing::info!("RETURNING table cell selection: span_idx={}, row={}, col={}", span_idx, row_idx, col_idx);
                    return Some(TableCellSelection {
                        table_index: span_idx,
                        row: row_idx,
                        col: col_idx,
                        cursor_in_cell,
                    });
                }
            }
        }

        tracing::info!("find_table_cell_at returning None - no table found at click position");
        None
    }

    /// Handle drag (extend selection)
    pub fn drag(&mut self, x: f32, y: f32, font_system: &mut FontSystem) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.cursor = self.position_from_point(x, y, font_system);
        self.needs_redraw = true;
        self.reset_blink();
    }

    /// Convert pixel position to text position using cosmic-text's hit testing
    /// Returns a position in the source text (with markdown markers)
    fn position_from_point(&self, x: f32, y: f32, font_system: &mut FontSystem) -> usize {
        // Apply zoom to metrics (must match render() exactly)
        let padding = self.config.padding;  // Padding doesn't scale with zoom
        let metrics = Metrics::new(self.config.font_size * self.zoom, self.config.line_height * self.zoom);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(self.width), Some(self.height));

        // Use the same rich text rendering as in render()
        let spans = parse_markdown_spans(&self.text);
        let rich_spans: Vec<(&str, Attrs)> = spans.iter().map(|span| {
            // Apply zoom to font sizes (must match render())
            let font_size = span.font_size.unwrap_or(self.config.font_size) * self.zoom;
            let line_height = font_size * (self.config.line_height / self.config.font_size);
            let mut attrs = Attrs::new()
                .family(self.config.font_family)
                .metrics(Metrics::new(font_size, line_height));
            if span.bold || span.heading_level.is_some() {
                attrs = attrs.weight(Weight::BOLD);
            }
            if span.italic {
                attrs = attrs.style(Style::Italic);
            }
            if span.code || span.is_block_code {
                attrs = attrs.family(Family::Monospace);
            }
            (span.text.as_str(), attrs)
        }).collect();

        buffer.set_rich_text(font_system, rich_spans, Attrs::new(), Shaping::Advanced);
        buffer.set_size(font_system, Some(self.width), None);
        buffer.set_scroll(cosmic_text::Scroll::default());
        buffer.shape_until_scroll(font_system, true);

        // Subtract padding from input coordinates to get content coordinates
        let content_x = (x - padding).max(0.0);
        let content_y = (y - padding).max(0.0) + self.scroll_y;

        // Use cosmic-text's built-in hit testing with content coordinates
        if let Some(cursor) = buffer.hit(content_x, content_y) {
            // Convert the Cursor to a byte index in display text
            let mut display_byte_pos = 0;
            for (i, line) in buffer.lines.iter().enumerate() {
                if i == cursor.line {
                    display_byte_pos += cursor.index;
                    break;
                }
                // Add the length of this line plus newline
                display_byte_pos += line.text().len() + 1; // +1 for newline
            }

            // Convert display position to source position
            let source_pos = self.display_to_source_position(display_byte_pos);
            tracing::debug!("Hit at ({}, {}) scroll_y={} content_y={} -> display_pos={}, source_pos={}",
                x, y, self.scroll_y, content_y, display_byte_pos, source_pos);
            source_pos.min(self.text.len())
        } else {
            tracing::debug!("No hit at ({}, {}) scroll_y={} content_y={}, defaulting to end", x, y, self.scroll_y, content_y);
            self.text.len()
        }
    }

    /// Render to pixel buffer with rich text styling, scroll, and zoom support
    pub fn render(&mut self, font_system: &mut FontSystem, swash_cache: &mut SwashCache) -> &PixelBuffer {
        if !self.needs_redraw && self.cached_buffer.is_some() {
            return self.cached_buffer.as_ref().unwrap();
        }

        let width = self.width.max(1.0) as u32;
        let height = self.height.max(1.0) as u32;
        let padding = self.config.padding;  // Padding doesn't scale with zoom

        let mut buffer = PixelBuffer::new(width, height);
        buffer.clear(self.config.background_color);

        // Apply zoom to base font size and line height
        let base_font_size = self.config.font_size * self.zoom;
        let base_line_height = self.config.line_height * self.zoom;

        // Create cosmic-text buffer with zoomed metrics
        // Reduce width by padding on both sides for text wrapping
        let metrics = Metrics::new(base_font_size, base_line_height);
        let mut text_buffer = Buffer::new(font_system, metrics);
        let content_width = (self.width - padding * 2.0).max(1.0);
        text_buffer.set_size(font_system, Some(content_width), Some(self.height));

        // Parse markdown into styled spans and build rich text (with zoom applied)
        let spans = parse_markdown_spans(&self.text);

        // Debug: print first render with actual content
        static RENDER_DEBUG: std::sync::Once = std::sync::Once::new();
        if !self.text.is_empty() && spans.len() > 1 {
            RENDER_DEBUG.call_once(|| {
                eprintln!("========== FIRST RENDER ==========");
                eprintln!("Editor text ({} chars): {:?}", self.text.len(),
                    if self.text.len() > 100 { &self.text[..100] } else { &self.text });
                eprintln!("Generated {} spans", spans.len());
                for (i, span) in spans.iter().take(10).enumerate() {
                    let text_preview = if span.text.len() > 40 {
                        format!("{}...", &span.text[..40])
                    } else {
                        span.text.clone()
                    };
                    eprintln!("  Span {}: text={:?} h={:?} list={:?} bold={} fs={:?}",
                        i, text_preview, span.heading_level, span.list_item, span.bold, span.font_size);
                }
                eprintln!("===================================");
            });
        }

        let default_color = self.config.text_color;
        let zoom = self.zoom;
        let rich_spans: Vec<(&str, Attrs)> = spans.iter().map(|span| {
            // Apply zoom to font sizes
            let font_size = span.font_size.unwrap_or(self.config.font_size) * zoom;
            // Calculate line height proportionally to font size
            let line_height = font_size * (self.config.line_height / self.config.font_size);
            let mut attrs = Attrs::new()
                .family(self.config.font_family)
                .metrics(Metrics::new(font_size, line_height));

            // Set text color
            if let Some(color) = span.text_color {
                attrs = attrs.color(color);
            } else {
                attrs = attrs.color(default_color);
            }

            if span.bold || span.heading_level.is_some() {
                // Headings are bold by default
                attrs = attrs.weight(Weight::BOLD);
            }
            if span.italic {
                attrs = attrs.style(Style::Italic);
            }
            if span.code || span.is_block_code {
                attrs = attrs.family(Family::Monospace);
            }

            (span.text.as_str(), attrs)
        }).collect();

        text_buffer.set_rich_text(font_system, rich_spans, Attrs::new(), Shaping::Advanced);

        // Set buffer with actual width for correct line wrapping, but unbounded height
        // so cosmic-text will draw ALL content, and we'll clip to viewport ourselves
        text_buffer.set_size(font_system, Some(self.width), None);

        // Shape ALL lines
        text_buffer.set_scroll(cosmic_text::Scroll::default());
        text_buffer.shape_until_scroll(font_system, true);  // Shape all

        // Calculate content height from all lines by tracking y position
        let mut content_height = 0.0f32;
        for line in text_buffer.lines.iter() {
            if let Some(layout) = line.layout_opt() {
                for layout_line in layout.iter() {
                    let line_h = layout_line.line_height_opt.unwrap_or(base_line_height);
                    content_height += line_h;
                }
            } else {
                // Line not shaped yet, estimate height
                content_height += base_line_height;
            }
        }
        self.cached_content_height = content_height;

        // Apply scroll offset for drawing
        let scroll_y = self.scroll_y;

        // For cursor/selection, we need the display text (without markers)
        let _display_text: String = spans.iter().map(|s| s.text.as_str()).collect();

        // Map cursor from source position to display position
        let display_cursor = self.source_to_display_position(self.cursor);
        let display_selection = self.selection().map(|(start, end)| {
            (self.source_to_display_position(start), self.source_to_display_position(end))
        });

        // Draw highlight backgrounds for spans that have background_color (with scroll and padding offset)
        self.draw_highlights_scrolled(&text_buffer, &mut buffer, &spans, scroll_y, padding);

        // Draw selection background using display positions (with scroll and padding offset)
        if let Some((sel_start, sel_end)) = display_selection {
            self.draw_selection_scrolled(&text_buffer, &mut buffer, sel_start, sel_end, scroll_y, padding);
        }

        // Draw cursor using display position (with scroll and padding offset)
        self.draw_cursor_scrolled(&text_buffer, &mut buffer, display_cursor, scroll_y, padding);

        // Draw text (using glyph colors set via attrs.color()) with scroll and padding offset
        let padding_i32 = padding as i32;
        let top_padding = padding as i32;
        text_buffer.draw(font_system, swash_cache, default_color, |x, y, w, h, color| {
            let scrolled_y = y - scroll_y as i32 + top_padding;
            // Only draw if visible in viewport
            if scrolled_y + (h as i32) > 0 && scrolled_y < height as i32 {
                buffer.fill_rect(x + padding_i32, scrolled_y, w, h, color);
            }
        });

        // Draw strikethrough lines for spans that have it (with scroll and padding offset)
        self.draw_strikethrough_scrolled(&text_buffer, &mut buffer, &spans, scroll_y, padding);

        // Draw tables with proper visual rendering (borders, cells)
        self.draw_tables_scrolled(&text_buffer, &mut buffer, &spans, scroll_y, font_system, swash_cache, padding);

        self.cached_buffer = Some(buffer);
        self.needs_redraw = false;
        self.cached_buffer.as_ref().unwrap()
    }

    /// Convert a position in the source text (with markers) to display text (without markers)
    /// Handles both block-level markers (headings, lists) and inline markers (bold, italic, etc.)
    fn source_to_display_position(&self, source_pos: usize) -> usize {
        let mut display_pos = 0;
        let mut source_byte_idx = 0;
        let lines: Vec<&str> = self.text.split('\n').collect();
        let mut line_idx = 0;

        while line_idx < lines.len() {
            let line = lines[line_idx];
            let line_start = source_byte_idx;
            let line_end = source_byte_idx + line.len();

            // Check for early exit (source_pos is at/before start of this line)
            if source_pos <= line_start && line_idx > 0 {
                break;
            }

            // Handle code fence (opening)
            if line.starts_with("```") {
                // Check if position is in the opening fence
                if source_pos >= line_start && source_pos <= line_end {
                    return display_pos;
                }

                // Opening fence produces NO display output (code content follows directly)
                let code_block_display_start = display_pos;
                let code_block_source_start = line_end + 1;
                source_byte_idx = line_end + 1;
                line_idx += 1;

                // Process code block content
                let mut code_content_source_len = 0usize;
                let mut first_content_line = true;
                while line_idx < lines.len() && !lines[line_idx].starts_with("```") {
                    let content_line = lines[line_idx];
                    let content_line_start = source_byte_idx;
                    let content_line_end = source_byte_idx + content_line.len();

                    // Check if source_pos is within this content line
                    if source_pos >= content_line_start && source_pos <= content_line_end {
                        // Calculate position within display code block
                        let offset_in_source = source_pos - code_block_source_start;
                        return code_block_display_start + offset_in_source;
                    }

                    if !first_content_line {
                        code_content_source_len += 1; // newline between content lines
                    }
                    code_content_source_len += content_line.len();
                    first_content_line = false;

                    source_byte_idx = content_line_end + 1;
                    line_idx += 1;
                }

                // Code block display length is source length minus trailing newline
                // (parse_markdown_spans removes the trailing newline from code block content)
                let code_display_len = code_content_source_len;
                display_pos += code_display_len;

                // Handle closing fence (if present)
                if line_idx < lines.len() && lines[line_idx].starts_with("```") {
                    let closing_line_start = source_byte_idx;
                    let closing_line_end = source_byte_idx + lines[line_idx].len();

                    // Check if source_pos is in closing fence
                    if source_pos >= closing_line_start && source_pos <= closing_line_end {
                        return display_pos;
                    }

                    source_byte_idx = closing_line_end + 1;

                    // Closing fence produces a newline
                    if line_idx < lines.len() - 1 {
                        display_pos += 1;
                    }
                    line_idx += 1;
                }
                continue;
            }

            // Normal line processing
            let (marker_len, display_prefix) = get_line_marker_info(line);

            display_pos += display_prefix.len();

            if source_pos >= line_start && source_pos < line_start + marker_len {
                return display_pos;
            }

            let content = &line[marker_len..];
            let content_start = line_start + marker_len;

            if source_pos >= content_start && source_pos <= line_end {
                let pos_in_content = source_pos - content_start;
                display_pos += source_to_display_inline(content, pos_in_content);
                return display_pos;
            }

            display_pos += source_to_display_inline(content, content.len());
            source_byte_idx = line_end + 1;

            if line_idx < lines.len() - 1 {
                display_pos += 1;
            }
            line_idx += 1;
        }

        display_pos
    }

    /// Convert a position in display text (without markers) to source text (with markers)
    fn display_to_source_position(&self, display_pos: usize) -> usize {
        let mut current_display_pos = 0;
        let mut source_byte_idx = 0;
        let lines: Vec<&str> = self.text.split('\n').collect();
        let mut line_idx = 0;

        while line_idx < lines.len() {
            let line = lines[line_idx];
            let line_start = source_byte_idx;
            let line_end = source_byte_idx + line.len();

            // Handle code fence (opening)
            if line.starts_with("```") {
                // If display_pos is at/before this position, return start of fence
                if current_display_pos >= display_pos {
                    return line_start;
                }

                // Opening fence produces NO display output (code content follows directly)
                let code_block_source_start = line_end + 1;
                source_byte_idx = line_end + 1;
                line_idx += 1;

                // Collect code block content
                let mut code_content_len = 0usize;
                let mut code_source_end = source_byte_idx;
                while line_idx < lines.len() && !lines[line_idx].starts_with("```") {
                    if code_content_len > 0 {
                        code_content_len += 1; // newline between lines
                    }
                    code_content_len += lines[line_idx].len();
                    code_source_end = source_byte_idx + lines[line_idx].len();
                    source_byte_idx = code_source_end + 1;
                    line_idx += 1;
                }

                // Check if display_pos is within code block content
                if current_display_pos + code_content_len > display_pos {
                    let offset = display_pos - current_display_pos;
                    return code_block_source_start + offset;
                }
                current_display_pos += code_content_len;

                // Handle closing fence (if present)
                if line_idx < lines.len() && lines[line_idx].starts_with("```") {
                    let closing_fence_start = source_byte_idx;
                    let closing_fence_end = source_byte_idx + lines[line_idx].len();
                    source_byte_idx = closing_fence_end + 1;

                    // Closing fence produces a newline
                    if line_idx < lines.len() - 1 {
                        current_display_pos += 1;
                    }
                    line_idx += 1;
                }
                continue;
            }

            // Normal line processing
            let (marker_len, display_prefix) = get_line_marker_info(line);

            // If display_pos is exactly at the start of this line's display, map to line start
            // This ensures Enter at start of a heading inserts before ## not after
            if display_pos == current_display_pos {
                return line_start;
            }

            // If display_pos is within the prefix, map to start of line content
            if current_display_pos + display_prefix.len() > display_pos {
                return line_start + marker_len;
            }

            current_display_pos += display_prefix.len();

            // Process inline content
            let content = &line[marker_len..];
            let content_display_len = source_to_display_inline(content, content.len());

            if current_display_pos + content_display_len >= display_pos {
                let target_in_content = display_pos - current_display_pos;
                return line_start + marker_len + display_to_source_inline(content, target_in_content);
            }

            current_display_pos += content_display_len;
            source_byte_idx = line_end + 1;

            // Account for newline in display
            if line_idx < lines.len() - 1 {
                current_display_pos += 1;
            }
            line_idx += 1;
        }

        self.text.len()
    }

    /// Draw cursor at a specific display position
    fn draw_cursor_at(&self, text_buffer: &Buffer, pixel_buffer: &mut PixelBuffer, display_cursor: usize) {
        let cursor_color = self.config.cursor_color;

        // Convert display byte offset to line and index within line
        let display_text: String = parse_markdown_spans(&self.text).iter().map(|s| s.text.as_str()).collect();

        let mut line_num = 0usize;
        let mut line_start_byte = 0usize;

        for (i, line) in text_buffer.lines.iter().enumerate() {
            let line_len = line.text().len();
            let line_end_byte = line_start_byte + line_len;

            if display_cursor <= line_end_byte {
                line_num = i;
                break;
            }
            line_start_byte = line_end_byte + 1; // +1 for newline
        }

        let index_in_line = display_cursor.saturating_sub(line_start_byte);

        // Find the x position by iterating through glyphs on this line
        let mut cursor_x = 0.0f32;
        let mut cursor_y = 0.0f32;
        let mut found = false;

        for run in text_buffer.layout_runs() {
            if run.line_i != line_num {
                continue;
            }

            cursor_y = run.line_y - self.config.font_size;

            if index_in_line == 0 {
                cursor_x = 0.0;
                found = true;
                break;
            }

            for glyph in run.glyphs.iter() {
                if glyph.start <= index_in_line && index_in_line <= glyph.end {
                    if index_in_line == glyph.start {
                        cursor_x = glyph.x;
                    } else {
                        cursor_x = glyph.x + glyph.w;
                    }
                    found = true;
                }
                if glyph.end <= index_in_line {
                    cursor_x = glyph.x + glyph.w;
                    found = true;
                }
            }
        }

        if !found {
            cursor_y = line_num as f32 * self.config.line_height;
        }

        pixel_buffer.fill_rect(
            cursor_x as i32,
            cursor_y as i32,
            2,
            self.config.line_height as u32,
            cursor_color,
        );
    }

    fn draw_selection(&self, text_buffer: &Buffer, pixel_buffer: &mut PixelBuffer, sel_start: usize, sel_end: usize) {
        let selection_color = self.config.selection_color;

        // Build a map of line start byte offsets
        let mut line_starts: Vec<usize> = Vec::new();
        let mut byte_pos = 0usize;
        for line in text_buffer.lines.iter() {
            line_starts.push(byte_pos);
            byte_pos += line.text().len() + 1; // +1 for newline
        }

        for run in text_buffer.layout_runs() {
            let line_y = run.line_y;
            let line_start = line_starts.get(run.line_i).copied().unwrap_or(0);
            let line_len = text_buffer.lines.get(run.line_i).map(|l| l.text().len()).unwrap_or(0);
            let line_end = line_start + line_len;

            // Check if this line overlaps with selection
            if sel_end <= line_start || sel_start >= line_end + 1 {
                continue; // No overlap
            }

            // Calculate selection bounds within this line
            let sel_start_in_line = sel_start.saturating_sub(line_start).min(line_len);
            let sel_end_in_line = sel_end.saturating_sub(line_start).min(line_len + 1);

            let mut sel_x_start = 0.0f32;
            let mut sel_x_end = 0.0f32;
            let mut found_start = sel_start_in_line == 0;
            let mut found_end = false;

            if sel_start_in_line == 0 {
                sel_x_start = 0.0;
            }

            for glyph in run.glyphs.iter() {
                // Check if selection start is at or before this glyph
                if !found_start && glyph.start >= sel_start_in_line {
                    sel_x_start = glyph.x;
                    found_start = true;
                }
                if !found_start && glyph.end > sel_start_in_line {
                    // Selection starts within this glyph
                    sel_x_start = glyph.x;
                    found_start = true;
                }

                // Check if selection end is at or before this glyph
                if glyph.start >= sel_end_in_line {
                    sel_x_end = glyph.x;
                    found_end = true;
                    break;
                }
                if glyph.end >= sel_end_in_line {
                    sel_x_end = glyph.x + glyph.w;
                    found_end = true;
                    break;
                }

                // Track the end in case selection goes to end of line
                sel_x_end = glyph.x + glyph.w;
            }

            if found_start {
                let width = (sel_x_end - sel_x_start).max(2.0) as u32;
                pixel_buffer.fill_rect(
                    sel_x_start as i32,
                    (line_y - self.config.font_size) as i32,
                    width,
                    self.config.line_height as u32,
                    selection_color,
                );
            }
        }
    }

    /// Draw strikethrough lines for spans that have strikethrough enabled
    fn draw_strikethrough(&self, text_buffer: &Buffer, pixel_buffer: &mut PixelBuffer, spans: &[StyledSpan]) {
        // Build display position map for strikethrough spans
        let mut display_pos = 0usize;
        let strikethrough_color = Color::rgb(0xA0, 0xA0, 0xA0);

        for span in spans {
            let span_len = span.text.len();
            if span.strikethrough && span_len > 0 {
                // Find the x positions for this span in the layout
                let start_pos = display_pos;
                let end_pos = display_pos + span_len;

                // Find which line(s) this span covers
                let mut line_byte_start = 0usize;
                for run in text_buffer.layout_runs() {
                    let line_len = text_buffer.lines.get(run.line_i)
                        .map(|l| l.text().len())
                        .unwrap_or(0);
                    let line_byte_end = line_byte_start + line_len;

                    // Check if span overlaps with this line
                    if start_pos < line_byte_end + 1 && end_pos > line_byte_start {
                        let span_start_in_line = start_pos.saturating_sub(line_byte_start).min(line_len);
                        let span_end_in_line = end_pos.saturating_sub(line_byte_start).min(line_len);

                        if span_start_in_line < span_end_in_line {
                            // Find x coordinates for the span in this line
                            let mut x_start = 0.0f32;
                            let mut x_end = 0.0f32;
                            let mut found_start = span_start_in_line == 0;

                            for glyph in run.glyphs.iter() {
                                if !found_start && glyph.end > span_start_in_line {
                                    x_start = glyph.x;
                                    found_start = true;
                                }
                                if glyph.end <= span_end_in_line {
                                    x_end = glyph.x + glyph.w;
                                }
                                if glyph.start >= span_end_in_line {
                                    break;
                                }
                            }

                            if found_start && x_end > x_start {
                                // Draw the strikethrough line in the middle of the text
                                let y = run.line_y - self.config.font_size * 0.35;
                                let width = (x_end - x_start) as u32;
                                pixel_buffer.fill_rect(
                                    x_start as i32,
                                    y as i32,
                                    width,
                                    1, // 1 pixel thick line
                                    strikethrough_color,
                                );
                            }
                        }
                    }

                    line_byte_start = line_byte_end + 1; // +1 for newline
                }
            }
            display_pos += span_len;
        }
    }

    /// Draw cursor with scroll offset applied
    fn draw_cursor_scrolled(&self, text_buffer: &Buffer, pixel_buffer: &mut PixelBuffer, display_cursor: usize, scroll_y: f32, padding: f32) {
        // Don't draw cursor if blinking is off
        if !self.cursor_visible {
            return;
        }
        let cursor_color = self.config.cursor_color;
        let zoomed_font_size = self.config.font_size * self.zoom;
        let zoomed_line_height = self.config.line_height * self.zoom;

        let mut line_num = 0usize;
        let mut line_start_byte = 0usize;

        for (i, line) in text_buffer.lines.iter().enumerate() {
            let line_len = line.text().len();
            let line_end_byte = line_start_byte + line_len;

            if display_cursor <= line_end_byte {
                line_num = i;
                break;
            }
            line_start_byte = line_end_byte + 1;
        }

        let index_in_line = display_cursor.saturating_sub(line_start_byte);

        let mut cursor_x = 0.0f32;
        let mut cursor_y = 0.0f32;
        let mut found = false;

        for run in text_buffer.layout_runs() {
            if run.line_i != line_num {
                continue;
            }

            cursor_y = run.line_y - zoomed_font_size;

            if index_in_line == 0 {
                cursor_x = 0.0;
                found = true;
                break;
            }

            for glyph in run.glyphs.iter() {
                if glyph.start <= index_in_line && index_in_line <= glyph.end {
                    if index_in_line == glyph.start {
                        cursor_x = glyph.x;
                    } else {
                        cursor_x = glyph.x + glyph.w;
                    }
                    found = true;
                }
                if glyph.end <= index_in_line {
                    cursor_x = glyph.x + glyph.w;
                    found = true;
                }
            }
        }

        if !found {
            cursor_y = line_num as f32 * zoomed_line_height;
        }

        // Apply scroll and padding offset
        let scrolled_y = (cursor_y - scroll_y + padding) as i32;

        // Only draw if visible
        if scrolled_y + zoomed_line_height as i32 > 0 && scrolled_y < pixel_buffer.height as i32 {
            pixel_buffer.fill_rect(
                (cursor_x + padding) as i32,
                scrolled_y,
                2,
                zoomed_line_height as u32,
                cursor_color,
            );
        }
    }

    /// Draw selection with scroll offset applied
    fn draw_selection_scrolled(&self, text_buffer: &Buffer, pixel_buffer: &mut PixelBuffer, sel_start: usize, sel_end: usize, scroll_y: f32, padding: f32) {
        let selection_color = self.config.selection_color;
        let zoomed_font_size = self.config.font_size * self.zoom;
        let zoomed_line_height = self.config.line_height * self.zoom;

        let mut line_starts: Vec<usize> = Vec::new();
        let mut byte_pos = 0usize;
        for line in text_buffer.lines.iter() {
            line_starts.push(byte_pos);
            byte_pos += line.text().len() + 1;
        }

        for run in text_buffer.layout_runs() {
            let line_y = run.line_y;
            let line_start = line_starts.get(run.line_i).copied().unwrap_or(0);
            let line_len = text_buffer.lines.get(run.line_i).map(|l| l.text().len()).unwrap_or(0);
            let line_end = line_start + line_len;

            if sel_end <= line_start || sel_start >= line_end + 1 {
                continue;
            }

            let sel_start_in_line = sel_start.saturating_sub(line_start).min(line_len);
            let sel_end_in_line = sel_end.saturating_sub(line_start).min(line_len + 1);

            let mut sel_x_start = 0.0f32;
            let mut sel_x_end = 0.0f32;
            let mut found_start = sel_start_in_line == 0;

            if sel_start_in_line == 0 {
                sel_x_start = 0.0;
            }

            for glyph in run.glyphs.iter() {
                if !found_start && glyph.start >= sel_start_in_line {
                    sel_x_start = glyph.x;
                    found_start = true;
                }
                if !found_start && glyph.end > sel_start_in_line {
                    sel_x_start = glyph.x;
                    found_start = true;
                }

                if glyph.start >= sel_end_in_line {
                    sel_x_end = glyph.x;
                    break;
                }
                if glyph.end >= sel_end_in_line {
                    sel_x_end = glyph.x + glyph.w;
                    break;
                }

                sel_x_end = glyph.x + glyph.w;
            }

            if found_start {
                let scrolled_y = (line_y - zoomed_font_size - scroll_y + padding) as i32;

                // Only draw if visible
                if scrolled_y + zoomed_line_height as i32 > 0 && scrolled_y < pixel_buffer.height as i32 {
                    let width = (sel_x_end - sel_x_start).max(2.0) as u32;
                    pixel_buffer.fill_rect(
                        (sel_x_start + padding) as i32,
                        scrolled_y,
                        width,
                        zoomed_line_height as u32,
                        selection_color,
                    );
                }
            }
        }
    }

    /// Draw strikethrough with scroll offset applied
    fn draw_strikethrough_scrolled(&self, text_buffer: &Buffer, pixel_buffer: &mut PixelBuffer, spans: &[StyledSpan], scroll_y: f32, padding: f32) {
        let mut display_pos = 0usize;
        // GitHub dark theme strikethrough color (dimmed text)
        let strikethrough_color = Color::rgb(0x8B, 0x94, 0x9E); // #8b949e
        let zoomed_font_size = self.config.font_size * self.zoom;

        for span in spans {
            let span_len = span.text.len();
            if span.strikethrough && span_len > 0 {
                let start_pos = display_pos;
                let end_pos = display_pos + span_len;

                let mut line_byte_start = 0usize;
                for run in text_buffer.layout_runs() {
                    let line_len = text_buffer.lines.get(run.line_i)
                        .map(|l| l.text().len())
                        .unwrap_or(0);
                    let line_byte_end = line_byte_start + line_len;

                    if start_pos < line_byte_end + 1 && end_pos > line_byte_start {
                        let span_start_in_line = start_pos.saturating_sub(line_byte_start).min(line_len);
                        let span_end_in_line = end_pos.saturating_sub(line_byte_start).min(line_len);

                        if span_start_in_line < span_end_in_line {
                            let mut x_start = 0.0f32;
                            let mut x_end = 0.0f32;
                            let mut found_start = span_start_in_line == 0;

                            if span_start_in_line == 0 {
                                x_start = 0.0;
                            }

                            for glyph in run.glyphs.iter() {
                                if !found_start && glyph.start >= span_start_in_line {
                                    x_start = glyph.x;
                                    found_start = true;
                                }
                                if !found_start && glyph.end > span_start_in_line {
                                    x_start = glyph.x;
                                    found_start = true;
                                }
                                if glyph.end >= span_end_in_line || glyph.start >= span_end_in_line {
                                    x_end = if glyph.start >= span_end_in_line { glyph.x } else { glyph.x + glyph.w };
                                    break;
                                }
                                x_end = glyph.x + glyph.w;
                            }

                            if found_start && x_end > x_start {
                                let y = run.line_y - zoomed_font_size * 0.35;
                                let scrolled_y = (y - scroll_y + padding) as i32;

                                // Only draw if visible (include y=0)
                                if scrolled_y >= 0 && scrolled_y < pixel_buffer.height as i32 {
                                    let width = (x_end - x_start) as u32;
                                    pixel_buffer.fill_rect(
                                        (x_start + padding) as i32,
                                        scrolled_y,
                                        width,
                                        2,  // Make strikethrough 2px thick for better visibility
                                        strikethrough_color,
                                    );
                                }
                            }
                        }
                    }

                    line_byte_start = line_byte_end + 1;
                }
            }
            display_pos += span_len;
        }
    }

    /// Draw highlight backgrounds with scroll offset applied
    fn draw_highlights_scrolled(&self, text_buffer: &Buffer, pixel_buffer: &mut PixelBuffer, spans: &[StyledSpan], scroll_y: f32, padding: f32) {
        let mut display_pos = 0usize;
        let zoomed_font_size = self.config.font_size * self.zoom;
        let zoomed_line_height = self.config.line_height * self.zoom;

        for span in spans {
            let span_len = span.text.len();
            if let Some(bg_color) = span.background_color {
                if span_len > 0 {
                    let start_pos = display_pos;
                    let end_pos = display_pos + span_len;
                    let is_code_block = span.is_block_code;

                    let mut line_byte_start = 0usize;
                    for run in text_buffer.layout_runs() {
                        let line_len = text_buffer.lines.get(run.line_i)
                            .map(|l| l.text().len())
                            .unwrap_or(0);
                        let line_byte_end = line_byte_start + line_len;

                        if start_pos < line_byte_end + 1 && end_pos > line_byte_start {
                            let span_start_in_line = start_pos.saturating_sub(line_byte_start).min(line_len);
                            let span_end_in_line = end_pos.saturating_sub(line_byte_start).min(line_len);

                            if span_start_in_line < span_end_in_line || is_code_block {
                                let y = run.line_y - zoomed_font_size;
                                let scrolled_y = (y - scroll_y + padding) as i32;

                                // Only draw if visible
                                if scrolled_y + zoomed_line_height as i32 > 0 && scrolled_y < pixel_buffer.height as i32 {
                                    if is_code_block {
                                        // For code blocks, draw full-width background
                                        let x = padding as i32;
                                        let width = (self.width - padding * 2.0).max(0.0) as u32;
                                        pixel_buffer.fill_rect(
                                            x,
                                            scrolled_y,
                                            width,
                                            zoomed_line_height as u32,
                                            bg_color,
                                        );
                                    } else {
                                        // For inline highlights, draw under text only
                                        let mut x_start = 0.0f32;
                                        let mut x_end = 0.0f32;
                                        let mut found_start = span_start_in_line == 0;

                                        if span_start_in_line == 0 {
                                            x_start = 0.0;
                                        }

                                        for glyph in run.glyphs.iter() {
                                            if !found_start && glyph.start >= span_start_in_line {
                                                x_start = glyph.x;
                                                found_start = true;
                                            }
                                            if !found_start && glyph.end > span_start_in_line {
                                                x_start = glyph.x;
                                                found_start = true;
                                            }
                                            if glyph.end >= span_end_in_line || glyph.start >= span_end_in_line {
                                                x_end = if glyph.start >= span_end_in_line { glyph.x } else { glyph.x + glyph.w };
                                                break;
                                            }
                                            x_end = glyph.x + glyph.w;
                                        }

                                        if found_start && x_end > x_start {
                                            let width = (x_end - x_start) as u32;
                                            pixel_buffer.fill_rect(
                                                (x_start + padding) as i32,
                                                scrolled_y,
                                                width,
                                                zoomed_line_height as u32,
                                                bg_color,
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        line_byte_start = line_byte_end + 1;
                    }
                }
            }
            display_pos += span_len;
        }
    }

    /// Draw tables with proper visual rendering (borders, grid, cell content)
    fn draw_tables_scrolled(
        &self,
        text_buffer: &Buffer,
        pixel_buffer: &mut PixelBuffer,
        spans: &[StyledSpan],
        scroll_y: f32,
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
        padding: f32,
    ) {
        let zoomed_font_size = self.config.font_size * self.zoom;
        let zoomed_line_height = self.config.line_height * self.zoom;
        // Use line height directly to match placeholder text sizing
        let cell_padding = 4.0 * self.zoom;
        // GitHub dark theme table colors
        let border_color = Color::rgb(0x30, 0x36, 0x3D);           // #30363d
        let header_bg_color = Color::rgba(0x16, 0x1B, 0x22, 0xFF); // #161b22
        let row_bg_color = Color::rgba(0x0D, 0x11, 0x17, 0xFF);    // #0d1117 (same as bg)
        let alt_row_bg_color = Color::rgba(0x16, 0x1B, 0x22, 0xFF);// #161b22

        let mut display_pos = 0usize;
        let selected_cell_color = Color::rgba(0x26, 0x4F, 0x78, 0x80); // GitHub selection blue
        let cell_cursor_color = Color::rgb(0x58, 0xA6, 0xFF);           // #58a6ff

        // Debug: show total spans and buffer info
        let total_display_len: usize = spans.iter().map(|s| s.text.len()).sum();
        let total_buffer_len: usize = text_buffer.lines.iter().map(|l| l.text().len()).sum();
        tracing::info!("draw_tables_scrolled: {} spans, total_display_len={}, buffer_lines={}, total_buffer_len={}",
            spans.len(), total_display_len, text_buffer.lines.len(), total_buffer_len);

        for (span_idx, span) in spans.iter().enumerate() {
            let span_len = span.text.len();

            if let Some(ref table) = span.table {
                // Find the y position of this table by searching for its placeholder text in the buffer
                // The placeholder is "[Table NxM]" at the start of the span
                let placeholder_start = &span.text[..span.text.find('\n').unwrap_or(span.text.len())];
                let mut table_y = 0.0f32;
                let mut found = false;

                // Search through buffer lines for the table placeholder
                for run in text_buffer.layout_runs() {
                    if let Some(line) = text_buffer.lines.get(run.line_i) {
                        let line_text = line.text();
                        if line_text.contains(placeholder_start) || line_text.starts_with("[Table") {
                            table_y = run.line_y - zoomed_font_size;
                            found = true;
                            tracing::info!("Table found via placeholder search at line {}, table_y={}", run.line_i, table_y);
                            break;
                        }
                    }
                }

                if !found {
                    tracing::warn!("Table placeholder not found in buffer!");
                    display_pos += span_len;
                    continue;
                }

                let scrolled_y = (table_y - scroll_y + padding) as i32;
                tracing::info!("Drawing table at scrolled_y={} (table_y={}, scroll_y={}, padding={})", scrolled_y, table_y, scroll_y, padding);

                // Calculate column widths based on content
                let num_cols = table.headers.len();
                let mut col_widths: Vec<f32> = vec![80.0 * self.zoom; num_cols]; // Minimum width

                // Measure header widths
                for (i, header) in table.headers.iter().enumerate() {
                    let text_width = self.measure_text_width(header, font_system, true);
                    if i < col_widths.len() {
                        col_widths[i] = col_widths[i].max(text_width + cell_padding * 2.0);
                    }
                }

                // Measure cell widths
                for row in &table.rows {
                    for (i, cell) in row.iter().enumerate() {
                        let text_width = self.measure_text_width(cell, font_system, false);
                        if i < col_widths.len() {
                            col_widths[i] = col_widths[i].max(text_width + cell_padding * 2.0);
                        }
                    }
                }

                let total_width: f32 = col_widths.iter().sum();
                // Row height matches line height to align with placeholder text
                let row_height = zoomed_line_height;
                let total_rows = 1 + table.rows.len(); // header + data rows
                let total_height = row_height * total_rows as f32;

                // Only draw if visible
                if scrolled_y + total_height as i32 > 0 && scrolled_y < pixel_buffer.height as i32 {
                    let table_x = padding as i32;

                    // Draw background to cover placeholder text
                    pixel_buffer.fill_rect(
                        table_x,
                        scrolled_y,
                        total_width as u32 + 2,
                        total_height as u32 + 2,
                        self.config.background_color,
                    );

                    // Draw header row background
                    pixel_buffer.fill_rect(
                        table_x,
                        scrolled_y,
                        total_width as u32,
                        row_height as u32,
                        header_bg_color,
                    );

                    // Draw data row backgrounds
                    for row_idx in 0..table.rows.len() {
                        let row_y = scrolled_y + ((row_idx + 1) as f32 * row_height) as i32;
                        let bg = if row_idx % 2 == 0 { row_bg_color } else { alt_row_bg_color };
                        pixel_buffer.fill_rect(
                            table_x,
                            row_y,
                            total_width as u32,
                            row_height as u32,
                            bg,
                        );
                    }

                    // Draw cell content
                    let mut cell_x = table_x as f32;

                    // Draw header cells
                    for (col_idx, header) in table.headers.iter().enumerate() {
                        let col_width = col_widths.get(col_idx).copied().unwrap_or(80.0);
                        let text_x = cell_x + cell_padding;
                        let text_y = scrolled_y as f32 + cell_padding / 2.0;

                        self.draw_cell_text(
                            pixel_buffer,
                            header,
                            text_x,
                            text_y,
                            col_width - cell_padding * 2.0,
                            true,
                            font_system,
                            swash_cache,
                        );

                        cell_x += col_width;
                    }

                    // Draw data cells
                    for (row_idx, row) in table.rows.iter().enumerate() {
                        cell_x = table_x as f32;
                        let row_y = scrolled_y as f32 + ((row_idx + 1) as f32 * row_height);

                        for (col_idx, cell) in row.iter().enumerate() {
                            let col_width = col_widths.get(col_idx).copied().unwrap_or(80.0);
                            let text_x = cell_x + cell_padding;
                            let text_y = row_y + cell_padding / 2.0;

                            self.draw_cell_text(
                                pixel_buffer,
                                cell,
                                text_x,
                                text_y,
                                col_width - cell_padding * 2.0,
                                false,
                                font_system,
                                swash_cache,
                            );

                            cell_x += col_width;
                        }
                    }

                    // Draw borders - horizontal lines
                    for row_idx in 0..=total_rows {
                        let line_y = scrolled_y + (row_idx as f32 * row_height) as i32;
                        pixel_buffer.fill_rect(table_x, line_y, total_width as u32, 1, border_color);
                    }

                    // Draw borders - vertical lines
                    let mut x = table_x as f32;
                    for col_width in col_widths.iter() {
                        pixel_buffer.fill_rect(x as i32, scrolled_y, 1, total_height as u32, border_color);
                        x += col_width;
                    }
                    pixel_buffer.fill_rect(x as i32, scrolled_y, 1, total_height as u32, border_color);

                    // Draw selected cell highlight if this table has a selected cell
                    if let Some(ref selection) = self.selected_table_cell {
                        if selection.table_index == span_idx {
                            // Calculate cell position
                            let sel_cell_x: f32 = col_widths[..selection.col].iter().sum();
                            let sel_cell_y = scrolled_y + (selection.row as f32 * row_height) as i32;
                            let sel_cell_width = col_widths.get(selection.col).copied().unwrap_or(80.0);

                            // Draw selection highlight
                            pixel_buffer.fill_rect(
                                sel_cell_x as i32 + 1,
                                sel_cell_y + 1,
                                sel_cell_width as u32 - 2,
                                row_height as u32 - 2,
                                selected_cell_color,
                            );

                            // Draw cell cursor if cursor is visible
                            if self.cursor_visible {
                                let cell_text = if selection.row == 0 {
                                    table.headers.get(selection.col).map(|s| s.as_str()).unwrap_or("")
                                } else {
                                    table.rows.get(selection.row - 1)
                                        .and_then(|row| row.get(selection.col))
                                        .map(|s| s.as_str())
                                        .unwrap_or("")
                                };

                                // Calculate cursor x position
                                let text_before_cursor = &cell_text[..selection.cursor_in_cell.min(cell_text.len())];
                                let cursor_x_offset = if !text_before_cursor.is_empty() {
                                    self.measure_text_width(text_before_cursor, font_system, selection.row == 0)
                                } else {
                                    0.0
                                };

                                let cursor_x = sel_cell_x + cell_padding + cursor_x_offset;
                                let cursor_y = sel_cell_y + 2;

                                pixel_buffer.fill_rect(
                                    cursor_x as i32,
                                    cursor_y,
                                    2,
                                    row_height as u32 - 4,
                                    cell_cursor_color,
                                );
                            }
                        }
                    }
                }
            }

            display_pos += span_len;
        }
    }

    /// Measure text width for table cell sizing
    fn measure_text_width(&self, text: &str, font_system: &mut FontSystem, bold: bool) -> f32 {
        let metrics = Metrics::new(self.config.font_size * self.zoom, self.config.line_height * self.zoom);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(1000.0), Some(50.0));

        let mut attrs = Attrs::new()
            .family(self.config.font_family)
            .metrics(metrics);
        if bold {
            attrs = attrs.weight(Weight::BOLD);
        }

        buffer.set_text(font_system, text, attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);

        // Calculate width from glyphs
        let mut width = 0.0f32;
        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                width = width.max(glyph.x + glyph.w);
            }
        }
        width
    }

    /// Draw text in a table cell
    fn draw_cell_text(
        &self,
        pixel_buffer: &mut PixelBuffer,
        text: &str,
        x: f32,
        y: f32,
        _max_width: f32,
        bold: bool,
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
    ) {
        let metrics = Metrics::new(self.config.font_size * self.zoom, self.config.line_height * self.zoom);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(1000.0), Some(50.0));

        let mut attrs = Attrs::new()
            .family(self.config.font_family)
            .metrics(metrics)
            .color(self.config.text_color);
        if bold {
            attrs = attrs.weight(Weight::BOLD);
        }

        buffer.set_text(font_system, text, attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);

        // Draw the text
        buffer.draw(font_system, swash_cache, self.config.text_color, |gx, gy, w, h, color| {
            let px = x as i32 + gx;
            let py = y as i32 + gy;
            if px >= 0 && py >= 0 && px < pixel_buffer.width as i32 && py < pixel_buffer.height as i32 {
                pixel_buffer.fill_rect(px, py, w, h, color);
            }
        });
    }
}

/// Global font system (thread-safe)
static FONT_SYSTEM: std::sync::OnceLock<Mutex<FontSystem>> = std::sync::OnceLock::new();

/// Get the global font system
pub fn get_font_system() -> &'static Mutex<FontSystem> {
    FONT_SYSTEM.get_or_init(|| Mutex::new(FontSystem::new()))
}

/// Global swash cache
static SWASH_CACHE: std::sync::OnceLock<Mutex<SwashCache>> = std::sync::OnceLock::new();

/// Get the global swash cache
pub fn get_swash_cache() -> &'static Mutex<SwashCache> {
    SWASH_CACHE.get_or_init(|| Mutex::new(SwashCache::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_markdown_bullet_list() {
        let input = "- one\n- two\n- three";
        let spans = parse_markdown_spans(input);

        // Print spans for debugging
        for (i, span) in spans.iter().enumerate() {
            println!("Span {}: text={:?} h={:?} bold={} fs={:?}",
                i, span.text, span.heading_level, span.bold, span.font_size);
        }

        // Check that bullets are replaced with bullet character
        let all_text: String = spans.iter().map(|s| s.text.as_str()).collect();
        println!("All text: {:?}", all_text);

        // Should have bullet prefix, not dash
        assert!(all_text.contains("•"), "Expected bullet character in: {}", all_text);
        assert!(!all_text.contains("- "), "Should not contain '- ' prefix in: {}", all_text);

        // List items should NOT have heading styling
        for span in &spans {
            if !span.text.trim().is_empty() && span.text != "\n" {
                assert!(span.heading_level.is_none(),
                    "List item '{}' should not have heading_level, got {:?}", span.text, span.heading_level);
                assert!(!span.bold, "List item '{}' should not be bold", span.text);
                assert!(span.font_size.is_none(),
                    "List item '{}' should not have explicit font_size, got {:?}", span.text, span.font_size);
            }
        }
    }

    #[test]
    fn test_parse_markdown_block_quote() {
        let input = "> quoted text";
        let spans = parse_markdown_spans(input);

        for (i, span) in spans.iter().enumerate() {
            println!("Span {}: {:?}", i, span.text);
        }

        let all_text: String = spans.iter().map(|s| s.text.as_str()).collect();
        println!("All text: {:?}", all_text);

        // Should have quote bar, not >
        assert!(all_text.contains("│"), "Expected quote bar in: {}", all_text);
        assert!(!all_text.starts_with(">"), "Should not start with > in: {}", all_text);
    }

    #[test]
    fn test_basic_editing() {
        let config = EditorConfig::default();
        let mut editor = SimpleCosmicEditor::new(config);

        editor.insert_char('H');
        editor.insert_char('i');
        assert_eq!(editor.text(), "Hi");
        assert_eq!(editor.cursor_position(), 2);

        editor.backspace();
        assert_eq!(editor.text(), "H");
        assert_eq!(editor.cursor_position(), 1);
    }

    #[test]
    fn test_cursor_movement() {
        let config = EditorConfig::default();
        let mut editor = SimpleCosmicEditor::new(config);

        editor.set_text("Hello");
        editor.move_right(false);
        assert_eq!(editor.cursor_position(), 1);

        editor.move_left(false);
        assert_eq!(editor.cursor_position(), 0);

        editor.move_end(false);
        assert_eq!(editor.cursor_position(), 5);

        editor.move_home(false);
        assert_eq!(editor.cursor_position(), 0);
    }
}
