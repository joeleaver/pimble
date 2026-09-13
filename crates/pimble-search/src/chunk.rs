//! `chunk_units`: groups a node's [`IndexUnit`]s into embeddable chunks.
//!
//! Pure function, no I/O: it never touches rhypedb. `SearchIndex::upsert`
//! calls it (when semantic search is enabled) and reconciles the result
//! against the `Chunk` objects already stored for a node.

use pimble_core::{IndexUnit, UnitKind};
use sha2::{Digest, Sha256};
use unicode_segmentation::UnicodeSegmentation;

use crate::schema::hex_encode;

/// Word budget for a chunk (roughly 256 word-pieces — `all-MiniLM-L6-v2`'s
/// training window).
const MAX_CHUNK_WORDS: usize = 200;
/// Overlap, in words, between consecutive pieces of one oversize unit split
/// by [`split_words_with_overlap`].
const OVERLAP_WORDS: usize = 30;

/// One embeddable piece of a node's content, ready to become (or update) a
/// `Chunk` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSpec {
    /// Stable position in the unit stream: `Chunk.ordinal`. Recomputing the
    /// chunk list for updated content reuses these positions so a re-index
    /// can diff old vs. new by ordinal instead of by content.
    pub ordinal: u32,
    /// One of `"prose"`, `"heading"`, `"code"`, `"table"`, `"field"`,
    /// `"other"` — the dominant [`UnitKind`] contributing to this chunk.
    pub kind: String,
    /// The locator of the chunk's first constituent unit (or, for a table
    /// row, of the row itself) — `Chunk.path`.
    pub path: String,
    /// The chunk's own text — `Chunk.text`, and what a search hit's snippet
    /// is drawn from.
    pub text: String,
    /// `"{title}\n{context}\n{text}"` where `context` is the nearest
    /// preceding heading, or (inside a table) the table's header row —
    /// `Chunk.source`, the `@vectorize` input, so the embedding carries
    /// section context that `text` alone would lose.
    pub source: String,
    /// Content hash of `text` alone (not `source`) — `Chunk.hash`. Identical
    /// `text` always hashes identically; re-indexing compares this per
    /// ordinal to skip re-embedding chunks whose text didn't change.
    pub hash: String,
}

/// Which family of unit an [`UnitKind`] chunks with. Units in different
/// categories never share a chunk; category also decides how an oversize
/// unit is split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    /// `Prose`, `Heading`, and `Other` all read as running text and freely
    /// merge together.
    Prose,
    Code,
    Table,
    Field,
}

fn category_of(kind: &UnitKind) -> Category {
    match kind {
        UnitKind::Code => Category::Code,
        UnitKind::TableRow => Category::Table,
        UnitKind::Field(_) => Category::Field,
        UnitKind::Prose | UnitKind::Heading(_) | UnitKind::Other(_) => Category::Prose,
    }
}

/// Group `units` (a node's content, in document order) into chunks of at most
/// [`MAX_CHUNK_WORDS`], splitting an oversize single unit into overlapping
/// (prose) or line-bounded (code) pieces. `title` seeds every chunk's
/// `source`. See the module docs and `docs/STEP5_CONTRACT.md` § Design for
/// the exact grouping rules.
pub fn chunk_units(title: &str, units: &[IndexUnit]) -> Vec<ChunkSpec> {
    let mut chunks = Vec::new();
    let mut ordinal: u32 = 0;
    let mut current_heading: Option<String> = None;
    let mut table_header: Option<String> = None;
    let mut buf: Vec<&IndexUnit> = Vec::new();
    let mut buf_category: Option<Category> = None;

    for unit in units {
        if let UnitKind::Heading(_) = &unit.kind {
            current_heading = Some(unit.text.clone());
        }
        let cat = category_of(&unit.kind);

        if cat == Category::Table && buf_category != Some(Category::Table) {
            // Entering a new contiguous table run: flush whatever came
            // before (with whatever context was valid then), then this row
            // becomes the header — context for the rows that follow, not
            // content of its own.
            flush(
                &mut buf,
                buf_category,
                &mut chunks,
                &mut ordinal,
                title,
                &current_heading,
                &table_header,
            );
            buf_category = Some(Category::Table);
            table_header = Some(unit.text.clone());
            continue;
        }

        let words = word_count(&unit.text);
        if words > MAX_CHUNK_WORDS {
            flush(
                &mut buf,
                buf_category,
                &mut chunks,
                &mut ordinal,
                title,
                &current_heading,
                &table_header,
            );
            buf_category = None;
            if cat != Category::Table {
                table_header = None;
            }
            for piece in split_oversize(unit, cat) {
                push_chunk(
                    &mut chunks,
                    &mut ordinal,
                    oversize_kind_label(&unit.kind),
                    unit.path.clone(),
                    piece,
                    &current_heading,
                    &table_header,
                    title,
                );
            }
            continue;
        }

        if buf_category == Some(cat) {
            let current_words: usize = buf.iter().map(|u| word_count(&u.text)).sum();
            if current_words + words > MAX_CHUNK_WORDS {
                flush(
                    &mut buf,
                    buf_category,
                    &mut chunks,
                    &mut ordinal,
                    title,
                    &current_heading,
                    &table_header,
                );
                buf_category = Some(cat);
            }
            buf.push(unit);
        } else {
            flush(
                &mut buf,
                buf_category,
                &mut chunks,
                &mut ordinal,
                title,
                &current_heading,
                &table_header,
            );
            buf_category = Some(cat);
            if cat != Category::Table {
                table_header = None;
            }
            buf.push(unit);
        }
    }
    flush(
        &mut buf,
        buf_category,
        &mut chunks,
        &mut ordinal,
        title,
        &current_heading,
        &table_header,
    );

    chunks
}

fn flush(
    buf: &mut Vec<&IndexUnit>,
    buf_category: Option<Category>,
    chunks: &mut Vec<ChunkSpec>,
    ordinal: &mut u32,
    title: &str,
    current_heading: &Option<String>,
    table_header: &Option<String>,
) {
    if buf.is_empty() {
        return;
    }
    let category = buf_category.expect("a non-empty buffer always carries a category");
    let kind = dominant_kind_label(buf, category);
    let path = buf[0].path.clone();
    let text = buf
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    push_chunk(
        chunks,
        ordinal,
        kind,
        path,
        text,
        current_heading,
        table_header,
        title,
    );
    buf.clear();
}

#[allow(clippy::too_many_arguments)]
fn push_chunk(
    chunks: &mut Vec<ChunkSpec>,
    ordinal: &mut u32,
    kind: &'static str,
    path: String,
    text: String,
    current_heading: &Option<String>,
    table_header: &Option<String>,
    title: &str,
) {
    let mut source = String::new();
    source.push_str(title);
    source.push('\n');
    if let Some(h) = table_header.as_ref().or(current_heading.as_ref()) {
        source.push_str(h);
        source.push('\n');
    }
    source.push_str(&text);

    let hash = content_hash(&text);
    chunks.push(ChunkSpec {
        ordinal: *ordinal,
        kind: kind.to_string(),
        path,
        text,
        source,
        hash,
    });
    *ordinal += 1;
}

fn dominant_kind_label(buf: &[&IndexUnit], category: Category) -> &'static str {
    match category {
        Category::Code => "code",
        Category::Table => "table",
        Category::Field => "field",
        Category::Prose => {
            let mut prose_words = 0usize;
            let mut heading_words = 0usize;
            let mut other_words = 0usize;
            for u in buf {
                let w = word_count(&u.text);
                match &u.kind {
                    UnitKind::Heading(_) => heading_words += w,
                    UnitKind::Other(_) => other_words += w,
                    _ => prose_words += w,
                }
            }
            if other_words > 0 && other_words >= prose_words && other_words >= heading_words {
                "other"
            } else if heading_words > prose_words {
                "heading"
            } else {
                "prose"
            }
        }
    }
}

fn oversize_kind_label(kind: &UnitKind) -> &'static str {
    match kind {
        UnitKind::Prose => "prose",
        UnitKind::Heading(_) => "heading",
        UnitKind::Code => "code",
        UnitKind::TableRow => "table",
        UnitKind::Field(_) => "field",
        UnitKind::Other(_) => "other",
    }
}

fn split_oversize(unit: &IndexUnit, category: Category) -> Vec<String> {
    match category {
        Category::Code => split_code_lines(&unit.text),
        _ => split_words_with_overlap(&unit.text),
    }
}

/// Split `text` on line boundaries only: accumulate whole lines into a piece
/// until the next line would push it over [`MAX_CHUNK_WORDS`]. A single line
/// longer than the budget still gets its own (oversize) piece rather than
/// being torn mid-line.
fn split_code_lines(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut pieces = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut current_words = 0usize;
    for line in lines {
        let lw = word_count(line);
        if !current.is_empty() && current_words + lw > MAX_CHUNK_WORDS {
            pieces.push(current.join("\n"));
            current = Vec::new();
            current_words = 0;
        }
        current.push(line);
        current_words += lw;
    }
    if !current.is_empty() || pieces.is_empty() {
        pieces.push(current.join("\n"));
    }
    pieces
}

/// Split `text` into pieces of at most [`MAX_CHUNK_WORDS`] words with
/// [`OVERLAP_WORDS`] words of overlap between consecutive pieces, cutting
/// only at word boundaries (never mid-word) and preserving every other
/// character (punctuation, whitespace) verbatim in whichever piece it falls
/// into.
fn split_words_with_overlap(text: &str) -> Vec<String> {
    let tokens: Vec<&str> = text.split_word_bounds().collect();
    let word_positions: Vec<usize> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.chars().next().is_some_and(|c| c.is_alphanumeric()))
        .map(|(i, _)| i)
        .collect();
    if word_positions.is_empty() {
        return vec![text.to_string()];
    }

    let mut pieces = Vec::new();
    let n = word_positions.len();
    let mut start_word_idx = 0usize;
    loop {
        let end_word_idx = (start_word_idx + MAX_CHUNK_WORDS).min(n);
        let token_start = word_positions[start_word_idx];
        let token_end = if end_word_idx == n {
            tokens.len()
        } else {
            word_positions[end_word_idx]
        };
        pieces.push(tokens[token_start..token_end].concat());
        if end_word_idx == n {
            break;
        }
        start_word_idx += MAX_CHUNK_WORDS - OVERLAP_WORDS;
    }
    pieces
}

fn word_count(text: &str) -> usize {
    text.unicode_words().count()
}

fn content_hash(text: &str) -> String {
    hex_encode(&Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(kind: UnitKind, path: &str, text: &str) -> IndexUnit {
        IndexUnit::new(kind, path, text)
    }

    fn words(n: usize) -> String {
        (1..=n).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn five_short_paragraphs_yield_one_chunk() {
        let units: Vec<IndexUnit> = (0..5)
            .map(|i| unit(UnitKind::Prose, &format!("b:{i}"), &words(20)))
            .collect();
        let chunks = chunk_units("Note", &units);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, "prose");
        assert_eq!(chunks[0].path, "b:0");
        assert!(chunks[0].source.starts_with("Note\n"));
    }

    #[test]
    fn oversize_prose_paragraph_splits_into_five_overlapping_pieces() {
        // 850 words: with a 200-word budget and 30-word overlap (170-word
        // stride) this tiles into exactly five pieces (4 full strides of 170
        // plus a closing window reaching the end).
        let text = words(850);
        let units = vec![unit(UnitKind::Prose, "b:0", &text)];
        let chunks = chunk_units("Doc", &units);
        assert_eq!(chunks.len(), 5);
        for c in &chunks {
            assert_eq!(c.kind, "prose");
            assert_eq!(c.path, "b:0");
        }
        // Consecutive pieces overlap: the tail of piece i reappears at the
        // head of piece i+1.
        for pair in chunks.windows(2) {
            let prev_words: Vec<&str> = pair[0].text.unicode_words().collect();
            let next_words: Vec<&str> = pair[1].text.unicode_words().collect();
            let overlap = &prev_words[prev_words.len() - OVERLAP_WORDS..];
            assert_eq!(overlap, &next_words[..OVERLAP_WORDS]);
        }
        // Every original word survives, in order, once each piece's leading
        // overlap (already checked above) is trimmed off.
        let mut reconstructed: Vec<&str> = chunks[0].text.unicode_words().collect();
        for c in &chunks[1..] {
            let piece_words: Vec<&str> = c.text.unicode_words().collect();
            reconstructed.extend_from_slice(&piece_words[OVERLAP_WORDS..]);
        }
        assert_eq!(reconstructed, text.unicode_words().collect::<Vec<_>>());
    }

    #[test]
    fn heading_then_paragraphs_source_starts_with_title_and_heading() {
        let units = vec![
            unit(UnitKind::Heading(2), "b:0", "Section One"),
            unit(UnitKind::Prose, "b:1", "First paragraph."),
            unit(UnitKind::Prose, "b:2", "Second paragraph."),
            unit(UnitKind::Prose, "b:3", "Third paragraph."),
        ];
        let chunks = chunk_units("My Doc", &units);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(
                c.source.starts_with("My Doc\nSection One\n"),
                "source was: {:?}",
                c.source
            );
        }
    }

    #[test]
    fn forty_line_code_block_splits_on_line_boundaries_never_mid_line() {
        let lines: Vec<String> = (0..40)
            .map(|i| {
                format!(
                    "let result_{i} = compute_something(alpha, beta, gamma, delta, epsilon, zeta_value);"
                )
            })
            .collect();
        let text = lines.join("\n");
        let units = vec![unit(UnitKind::Code, "b:0", &text)];
        let chunks = chunk_units("Code Doc", &units);
        assert!(chunks.len() > 1, "expected the code block to split");
        for c in &chunks {
            assert_eq!(c.kind, "code");
            // Every line in the piece must exactly match one of the original
            // lines — never a fragment of one.
            for piece_line in c.text.split('\n') {
                assert!(
                    lines.iter().any(|l| l == piece_line),
                    "line {piece_line:?} is not a whole original line"
                );
            }
        }
        // No line lost or duplicated across pieces (code pieces don't overlap).
        let mut reconstructed = Vec::new();
        for c in &chunks {
            reconstructed.extend(c.text.split('\n').map(str::to_string));
        }
        assert_eq!(reconstructed, lines);
    }

    #[test]
    fn table_rows_yield_chunks_whose_source_begins_with_header_row() {
        let mut units = vec![unit(UnitKind::TableRow, "b:0/r:0", "Name | Age | City")];
        for i in 0..10 {
            units.push(unit(
                UnitKind::TableRow,
                &format!("b:0/r:{}", i + 1),
                &format!("Name: {} | Age: {} | City: {}", words(20), i, words(20)),
            ));
        }
        let chunks = chunk_units("Table Doc", &units);
        assert!(chunks.len() > 1, "expected more than one row chunk");
        for c in &chunks {
            assert_eq!(c.kind, "table");
            assert!(
                c.source.starts_with("Table Doc\nName | Age | City\n"),
                "source was: {:?}",
                c.source
            );
        }
    }

    #[test]
    fn field_units_yield_field_kind_chunks_with_field_paths() {
        let units = vec![
            unit(UnitKind::Field("address.street".into()), "f:address.street", &words(90)),
            unit(UnitKind::Field("address.city".into()), "f:address.city", &words(90)),
            unit(UnitKind::Field("notes".into()), "f:notes", &words(90)),
        ];
        let chunks = chunk_units("Contact", &units);
        // Two short fields (90 + 90 = 180 words) fit in one chunk; the third
        // (would make 270) starts a new one — "one chunk per field unless
        // several short fields fit in one".
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].kind, "field");
        assert_eq!(chunks[0].path, "f:address.street");
        assert_eq!(chunks[1].kind, "field");
        assert_eq!(chunks[1].path, "f:notes");
    }

    #[test]
    fn identical_input_yields_identical_hashes() {
        let units = vec![unit(UnitKind::Prose, "b:0", "The quick brown fox.")];
        let a = chunk_units("Doc", &units);
        let b = chunk_units("Doc", &units);
        assert_eq!(a[0].hash, b[0].hash);
        assert_eq!(a[0].hash.len(), 64);
    }

    #[test]
    fn different_text_yields_different_hash() {
        let a = chunk_units("Doc", &[unit(UnitKind::Prose, "b:0", "alpha")]);
        let b = chunk_units("Doc", &[unit(UnitKind::Prose, "b:0", "beta")]);
        assert_ne!(a[0].hash, b[0].hash);
    }

    #[test]
    fn empty_units_yield_no_chunks() {
        assert!(chunk_units("Doc", &[]).is_empty());
    }
}
