//! RTF-to-BlockData converter
//!
//! Handles the subset of RTF used by Scrivener: extracts text with
//! bold, italic, and hyperlink formatting into EditorDocument blocks.

use std::collections::HashMap;

use rinch_core::ce::{BlockData, InlineMarkData, InlineRunData};

/// Active formatting state while parsing RTF.
#[derive(Clone, Default)]
struct FormatState {
    bold: bool,
    italic: bool,
    underline: bool,
    hyperlink: Option<String>, // Some(url) when inside a HYPERLINK field
}

/// Convert RTF data into a list of BlockData paragraphs.
pub fn rtf_to_blocks(rtf: &[u8]) -> Vec<BlockData> {
    let input = String::from_utf8_lossy(rtf);
    let mut blocks: Vec<BlockData> = Vec::new();
    let mut current_runs: Vec<InlineRunData> = Vec::new();
    let mut current_text = String::new();
    let mut fmt = FormatState::default();
    let mut fmt_stack: Vec<FormatState> = Vec::new();

    let mut chars = input.chars().peekable();
    let mut depth: i32 = 0;
    let mut skip_depth: Option<i32> = None;
    // Track HYPERLINK field parsing
    let mut field_depth: Option<i32> = None;
    let mut field_url: Option<String> = None;
    let mut in_fldrslt = false;

    while let Some(ch) = chars.next() {
        match ch {
            '{' => {
                depth += 1;
                fmt_stack.push(fmt.clone());

                if skip_depth.is_none() {
                    let rest: String = chars.clone().take(30).collect();
                    if rest.starts_with("\\fonttbl")
                        || rest.starts_with("\\colortbl")
                        || rest.starts_with("\\stylesheet")
                        || rest.starts_with("\\info")
                        || rest.starts_with("\\header")
                        || rest.starts_with("\\footer")
                    {
                        skip_depth = Some(depth);
                    } else if rest.starts_with("\\*\\") && !rest.starts_with("\\*\\fldinst") {
                        skip_depth = Some(depth);
                    } else if rest.starts_with("\\field") {
                        field_depth = Some(depth);
                    } else if rest.starts_with("\\*\\fldinst") {
                        // We'll parse the HYPERLINK URL from this group
                    } else if rest.starts_with("\\fldrslt") {
                        // The visible text of the hyperlink
                        if let Some(url) = &field_url {
                            flush_run(&mut current_text, &fmt, &mut current_runs);
                            fmt.hyperlink = Some(url.clone());
                            in_fldrslt = true;
                        }
                    }
                }
            }
            '}' => {
                if skip_depth == Some(depth) {
                    skip_depth = None;
                }
                if in_fldrslt && field_depth == Some(depth - 1) {
                    // End of fldrslt — stop hyperlink
                    flush_run(&mut current_text, &fmt, &mut current_runs);
                    fmt.hyperlink = None;
                    in_fldrslt = false;
                }
                if field_depth == Some(depth) {
                    field_depth = None;
                    field_url = None;
                }
                if let Some(prev) = fmt_stack.pop() {
                    let had_link = fmt.hyperlink.is_some();
                    let has_link = prev.hyperlink.is_some();
                    if fmt.bold != prev.bold
                        || fmt.italic != prev.italic
                        || fmt.underline != prev.underline
                        || had_link != has_link
                    {
                        flush_run(&mut current_text, &fmt, &mut current_runs);
                    }
                    fmt = prev;
                }
                depth -= 1;
            }
            '\\' if skip_depth.is_none() => {
                if let Some(&next) = chars.peek() {
                    if next == '\'' {
                        chars.next();
                        let hex: String = chars.by_ref().take(2).collect();
                        if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                            current_text.push(decode_windows_1252(byte));
                        }
                    } else if next == '\\' {
                        chars.next();
                        current_text.push('\\');
                    } else if next == '{' {
                        chars.next();
                        current_text.push('{');
                    } else if next == '}' {
                        chars.next();
                        current_text.push('}');
                    } else if next == '~' {
                        chars.next();
                        current_text.push('\u{00A0}');
                    } else if next == '\n' || next == '\r' {
                        chars.next();
                    } else if next.is_ascii_alphabetic() {
                        let (word, param) = read_control_word(&mut chars);
                        match word.as_str() {
                            "par" | "line" => {
                                flush_run(&mut current_text, &fmt, &mut current_runs);
                                finish_block(&mut current_runs, &mut blocks);
                            }
                            "tab" => current_text.push('\t'),
                            "b" => {
                                let on = param != Some(0);
                                if fmt.bold != on {
                                    flush_run(&mut current_text, &fmt, &mut current_runs);
                                    fmt.bold = on;
                                }
                            }
                            "i" => {
                                let on = param != Some(0);
                                if fmt.italic != on {
                                    flush_run(&mut current_text, &fmt, &mut current_runs);
                                    fmt.italic = on;
                                }
                            }
                            "ul" => {
                                if !fmt.underline {
                                    flush_run(&mut current_text, &fmt, &mut current_runs);
                                    fmt.underline = true;
                                }
                            }
                            "ulnone" => {
                                if fmt.underline {
                                    flush_run(&mut current_text, &fmt, &mut current_runs);
                                    fmt.underline = false;
                                }
                            }
                            "plain" => {
                                // Reset formatting
                                flush_run(&mut current_text, &fmt, &mut current_runs);
                                fmt.bold = false;
                                fmt.italic = false;
                                fmt.underline = false;
                                // Keep hyperlink if in field result
                            }
                            "fldinst" => {
                                // Parse HYPERLINK "url" from the field instruction
                                // Consume text until }
                                let mut inst = String::new();
                                for c in chars.by_ref() {
                                    if c == '}' {
                                        depth -= 1;
                                        if let Some(prev) = fmt_stack.pop() {
                                            fmt = prev;
                                        }
                                        break;
                                    }
                                    inst.push(c);
                                }
                                if let Some(url) = parse_hyperlink_url(&inst) {
                                    field_url = Some(url);
                                }
                            }
                            "fldrslt" => {
                                if let Some(url) = &field_url {
                                    flush_run(&mut current_text, &fmt, &mut current_runs);
                                    fmt.hyperlink = Some(url.clone());
                                    in_fldrslt = true;
                                }
                            }
                            _ => {
                                // Consume space delimiter
                                if chars.peek() == Some(&' ') {
                                    chars.next();
                                }
                            }
                        }
                    } else {
                        chars.next();
                    }
                }
            }
            '\n' | '\r' if skip_depth.is_none() => {}
            _ if skip_depth.is_none() => {
                current_text.push(ch);
            }
            _ => {}
        }
    }

    // Flush remaining content
    flush_run(&mut current_text, &fmt, &mut current_runs);
    finish_block(&mut current_runs, &mut blocks);

    // Strip Scrivener tags from all text runs
    for block in &mut blocks {
        for run in &mut block.content {
            run.text = strip_scrivener_tags(&run.text);
        }
    }

    // Remove empty trailing blocks
    while blocks.last().map_or(false, |b| {
        b.content.is_empty() || b.content.iter().all(|r| r.text.is_empty())
    }) {
        blocks.pop();
    }

    blocks
}

/// Legacy plain-text extraction (used for display labels).
pub fn rtf_to_text(rtf: &[u8]) -> String {
    let blocks = rtf_to_blocks(rtf);
    blocks
        .iter()
        .map(|b| {
            b.content
                .iter()
                .map(|r| r.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Flush accumulated text into a run with current formatting.
fn flush_run(text: &mut String, fmt: &FormatState, runs: &mut Vec<InlineRunData>) {
    if text.is_empty() {
        return;
    }
    let mut marks = Vec::new();
    if fmt.bold {
        marks.push(InlineMarkData {
            mark_type: "bold".to_string(),
            attrs: HashMap::new(),
        });
    }
    if fmt.italic {
        marks.push(InlineMarkData {
            mark_type: "italic".to_string(),
            attrs: HashMap::new(),
        });
    }
    if fmt.underline {
        marks.push(InlineMarkData {
            mark_type: "underline".to_string(),
            attrs: HashMap::new(),
        });
    }
    if let Some(url) = &fmt.hyperlink {
        let mut attrs = HashMap::new();
        attrs.insert("href".to_string(), url.clone());
        marks.push(InlineMarkData {
            mark_type: "link".to_string(),
            attrs,
        });
    }
    runs.push(InlineRunData {
        text: std::mem::take(text),
        marks,
    });
}

/// Finish the current paragraph block and start a new one.
fn finish_block(runs: &mut Vec<InlineRunData>, blocks: &mut Vec<BlockData>) {
    let content = std::mem::take(runs);
    blocks.push(BlockData {
        block_type: "paragraph".to_string(),
        attrs: HashMap::new(),
        content,
    });
}

/// Extract URL from a HYPERLINK field instruction like `HYPERLINK "https://example.com"`.
fn parse_hyperlink_url(inst: &str) -> Option<String> {
    let trimmed = inst.trim();
    // Skip any RTF control words in the instruction
    let text = if let Some(pos) = trimmed.find("HYPERLINK") {
        &trimmed[pos + 9..]
    } else {
        return None;
    };
    let text = text.trim();
    // URL is usually quoted
    if text.starts_with('"') {
        let end = text[1..].find('"')?;
        Some(text[1..1 + end].to_string())
    } else {
        // Unquoted — take until whitespace
        Some(text.split_whitespace().next()?.to_string())
    }
}

/// Strip Scrivener inline placeholder tags like `<$Scr_Ps::0>`.
fn strip_scrivener_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<$Scr_") {
        result.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find('>') {
            rest = &rest[start + end + 1..];
        } else {
            rest = &rest[start..];
            break;
        }
    }
    result.push_str(rest);
    result
}

/// Read a control word and its optional numeric parameter.
fn read_control_word(
    chars: &mut std::iter::Peekable<std::str::Chars>,
) -> (String, Option<i32>) {
    let mut word = String::new();
    while let Some(&ch) = chars.peek() {
        if ch.is_ascii_alphabetic() {
            word.push(ch);
            chars.next();
        } else {
            break;
        }
    }

    // Read optional numeric parameter (possibly negative)
    let mut param_str = String::new();
    if let Some(&ch) = chars.peek() {
        if ch == '-' || ch.is_ascii_digit() {
            param_str.push(ch);
            chars.next();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    param_str.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
        }
    }

    let param = if param_str.is_empty() {
        None
    } else {
        param_str.parse().ok()
    };

    (word, param)
}

/// Decode a Windows-1252 byte to a Unicode char.
fn decode_windows_1252(byte: u8) -> char {
    match byte {
        0x80 => '\u{20AC}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8E => '\u{017D}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        b => b as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_paragraphs() {
        let rtf = br#"{\rtf1\ansi\deff0
{\fonttbl{\f0\fnil Calibri;}}
{\colortbl;\red0\green0\blue0;}
\pard Hello World\par Second line}"#;
        let blocks = rtf_to_blocks(rtf);
        assert!(blocks.len() >= 2);
        let text: String = blocks[0].content.iter().map(|r| r.text.as_str()).collect();
        assert!(text.contains("Hello World"));
    }

    #[test]
    fn test_bold_italic() {
        let rtf = br"{\rtf1 Normal {\b bold} and {\i italic} text}";
        let blocks = rtf_to_blocks(rtf);
        assert!(!blocks.is_empty());
        let has_bold = blocks[0]
            .content
            .iter()
            .any(|r| r.marks.iter().any(|m| m.mark_type == "bold"));
        let has_italic = blocks[0]
            .content
            .iter()
            .any(|r| r.marks.iter().any(|m| m.mark_type == "italic"));
        assert!(has_bold, "should have bold run");
        assert!(has_italic, "should have italic run");
    }

    #[test]
    fn test_hyperlink() {
        let rtf = br#"{\rtf1 Click {\field{\*\fldinst HYPERLINK "https://example.com"}{\fldrslt here}} now}"#;
        let blocks = rtf_to_blocks(rtf);
        let has_link = blocks[0].content.iter().any(|r| {
            r.marks
                .iter()
                .any(|m| m.mark_type == "link" && m.attrs.get("href").map_or(false, |h| h.contains("example.com")))
        });
        assert!(has_link, "should have link mark");
    }

    #[test]
    fn test_scrivener_tags_stripped() {
        let rtf = br"{\rtf1 <$Scr_Ps::0>Hello World}";
        let blocks = rtf_to_blocks(rtf);
        let text: String = blocks
            .iter()
            .flat_map(|b| b.content.iter().map(|r| r.text.as_str()))
            .collect();
        assert!(!text.contains("Scr_Ps"));
        assert!(text.contains("Hello World"));
    }
}
