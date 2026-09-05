//! Serialising a [`Document`] to markdown.
//!
//! Markdown is the boundary between layer 3 (extraction) and layer 4 (ebook
//! building), and it is deliberately a real file on disk: it can be inspected,
//! diffed, and hand-corrected before the EPUB is built.
//!
//! The format is plain CommonMark with two conventions:
//!
//! * YAML front matter carries the metadata the EPUB needs.
//! * `<!-- page: 41 -->` marks a printed page boundary. An HTML comment because
//!   it has to survive round-tripping and stay invisible in any markdown
//!   viewer, and because page boundaries usually fall *inside* a paragraph, so
//!   the marker has to be legal inline.

use pdf_to_ebook_core::{Block, Document, Span};

pub fn page_marker(label: &str) -> String {
    format!("<!-- page: {label} -->")
}

pub fn write(doc: &Document) -> String {
    let mut out = String::new();

    out.push_str("---\n");
    out.push_str(&format!("title: {}\n", yaml_scalar(&doc.meta.title)));
    if let Some(a) = &doc.meta.author {
        out.push_str(&format!("author: {}\n", yaml_scalar(a)));
    }
    out.push_str(&format!("language: {}\n", yaml_scalar(&doc.meta.language)));
    if let Some(s) = &doc.meta.source {
        out.push_str(&format!("source: {}\n", yaml_scalar(s)));
    }
    if let Some(p) = doc.meta.page_count {
        out.push_str(&format!("pages: {p}\n"));
    }
    if let Some(g) = &doc.meta.generator {
        out.push_str(&format!("generator: {}\n", yaml_scalar(g)));
    }
    out.push_str("---\n\n");

    for b in &doc.blocks {
        match b {
            Block::Heading { level, text } => {
                let hashes = "#".repeat((*level).clamp(1, 6) as usize);
                out.push_str(&format!("{hashes} {}\n\n", escape_block(text)));
            }
            Block::Paragraph { spans } => {
                let mut line = String::new();
                for s in spans {
                    match s {
                        Span::Text(t) => line.push_str(&escape_inline(t)),
                        Span::Emphasis(t) => {
                            line.push('*');
                            line.push_str(&escape_inline(t));
                            line.push('*');
                        }
                        Span::PageBreak { label } => line.push_str(&page_marker(label)),
                    }
                }
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    out.push_str(trimmed);
                    out.push_str("\n\n");
                }
            }
            Block::PageBreak { label } => {
                out.push_str(&page_marker(label));
                out.push_str("\n\n");
            }
            Block::Separator => out.push_str("* * *\n\n"),
        }
    }
    out
}

fn yaml_scalar(s: &str) -> String {
    let needs_quotes = s.is_empty()
        || s.contains(':')
        || s.contains('#')
        || s.starts_with(['[', '{', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`'])
        || s.trim() != s;
    if needs_quotes {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// A heading must not accidentally continue past its line.
fn escape_block(s: &str) -> String {
    s.replace('\n', " ").trim().to_string()
}

/// Escape only what would change the block structure. Scanned prose is full of
/// `*` and `_` used as real punctuation, and over-escaping makes the markdown
/// unreadable for the human who may want to fix it by hand.
fn escape_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, ch) in s.char_indices() {
        match ch {
            // A line that starts with `#` would become a heading.
            '#' if i == 0 => out.push_str("\\#"),
            _ => out.push(ch),
        }
    }
    out.replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_to_ebook_core::Meta;

    fn doc() -> Document {
        Document {
            meta: Meta {
                title: "Kocamin Karisi".to_string(),
                author: Some("Jane Corry".to_string()),
                language: "tr".to_string(),
                source: Some("book.pdf".to_string()),
                page_count: Some(419),
                generator: Some("pdf-to-ebook 0.1.0".to_string()),
            },
            blocks: vec![
                Block::PageBreak { label: "301".to_string() },
                Block::Heading { level: 1, text: "44".to_string() },
                Block::Heading { level: 2, text: "Carla".to_string() },
                Block::Paragraph {
                    spans: vec![
                        Span::text("Tabii ki yeni resmin tanitimi"),
                        Span::PageBreak { label: "302".to_string() },
                        Span::text(" onlari bir araya getirmekte."),
                    ],
                },
            ],
        }
    }

    #[test]
    fn emits_front_matter_and_structure() {
        let md = write(&doc());
        assert!(md.starts_with("---\n"));
        assert!(md.contains("title: Kocamin Karisi\n"));
        assert!(md.contains("language: tr\n"));
        assert!(md.contains("pages: 419\n"));
        assert!(md.contains("# 44\n"));
        assert!(md.contains("## Carla\n"));
    }

    #[test]
    fn page_break_inside_a_paragraph_stays_inline() {
        let md = write(&doc());
        let para = md
            .lines()
            .find(|l| l.contains("Tabii ki"))
            .expect("paragraph line");
        // One line, marker in the middle: the paragraph was not split.
        assert!(para.contains("<!-- page: 302 -->"), "got {para:?}");
        assert!(para.ends_with("bir araya getirmekte."), "got {para:?}");
    }

    #[test]
    fn a_title_with_a_colon_is_quoted() {
        let mut d = doc();
        d.meta.title = "Dune: Part Two".to_string();
        assert!(write(&d).contains("title: \"Dune: Part Two\"\n"));
    }

    #[test]
    fn a_paragraph_starting_with_hash_is_escaped() {
        let d = Document {
            meta: Meta { title: "t".into(), language: "en".into(), ..Default::default() },
            blocks: vec![Block::Paragraph { spans: vec![Span::text("#1 bestseller")] }],
        };
        let md = write(&d);
        assert!(md.contains("\\#1 bestseller"), "got {md:?}");
    }
}
