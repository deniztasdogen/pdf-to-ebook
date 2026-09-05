//! Parsing the markdown produced by layer 3 back into a [`Document`].
//!
//! This is a real parse of a file on disk, not an in-memory handoff. That is
//! deliberate: the markdown can be hand-corrected between extraction and ebook
//! building, so this has to tolerate a human having edited it — extra blank
//! lines, reordered front matter, setext headings, missing fields.

use pdf_to_ebook_core::{Block, Document, Error, Meta, Result, Span};

/// `<!-- page: 41 -->`
fn find_page_marker(s: &str) -> Option<(usize, usize, String)> {
    let start = s.find("<!--")?;
    let rel_end = s[start..].find("-->")?;
    let end = start + rel_end + 3;
    let inner = &s[start + 4..start + rel_end];
    let t = inner.trim();
    let rest = t.strip_prefix("page:").or_else(|| t.strip_prefix("page"))?;
    Some((start, end, rest.trim().to_string()))
}

fn parse_inline(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some((start, end, label)) = find_page_marker(rest) {
        let before = &rest[..start];
        if !before.is_empty() {
            push_text(&mut spans, before);
        }
        spans.push(Span::PageBreak { label });
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        push_text(&mut spans, rest);
    }
    spans
}

/// Split a run of plain text into text and `*emphasis*` spans, and undo the
/// escaping the writer applied.
fn push_text(spans: &mut Vec<Span>, text: &str) {
    let unescaped = text.replace("\\#", "#");
    let mut rest = unescaped.as_str();
    loop {
        // Emphasis needs a closing marker on the same run; a lone `*` in
        // scanned prose is just punctuation.
        let Some(a) = rest.find('*') else { break };
        let Some(rel_b) = rest[a + 1..].find('*') else { break };
        let b = a + 1 + rel_b;
        let inner = &rest[a + 1..b];
        if inner.is_empty() || inner.contains('\n') {
            break;
        }
        if a > 0 {
            spans.push(Span::Text(rest[..a].to_string()));
        }
        spans.push(Span::Emphasis(inner.to_string()));
        rest = &rest[b + 1..];
    }
    if !rest.is_empty() {
        spans.push(Span::Text(rest.to_string()));
    }
}

fn is_separator(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    if t.len() < 3 {
        return false;
    }
    t.chars().all(|c| c == '*') || t.chars().all(|c| c == '-') || t.chars().all(|c| c == '_')
}

pub fn parse(md: &str) -> Result<Document> {
    let mut meta = Meta::default();
    let mut lines: Vec<&str> = md.lines().collect();

    // Front matter, if present.
    if lines.first().map(|l| l.trim() == "---").unwrap_or(false) {
        let mut i = 1;
        let mut closed = false;
        while i < lines.len() {
            if lines[i].trim() == "---" {
                closed = true;
                break;
            }
            if let Some((k, v)) = lines[i].split_once(':') {
                let key = k.trim().to_lowercase();
                let val = unquote(v.trim());
                match key.as_str() {
                    "title" => meta.title = val,
                    "author" => meta.author = Some(val),
                    "language" | "lang" => meta.language = val,
                    "source" => meta.source = Some(val),
                    "pages" => meta.page_count = val.parse().ok(),
                    "generator" => meta.generator = Some(val),
                    _ => {}
                }
            }
            i += 1;
        }
        if !closed {
            return Err(Error::Markdown {
                line: 1,
                reason: "front matter opened with `---` but never closed".to_string(),
            });
        }
        lines.drain(..=i);
    }

    if meta.title.trim().is_empty() {
        meta.title = "Untitled".to_string();
    }
    if meta.language.trim().is_empty() {
        meta.language = "und".to_string();
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut para: Vec<String> = Vec::new();

    let flush = |para: &mut Vec<String>, blocks: &mut Vec<Block>| {
        if para.is_empty() {
            return;
        }
        let joined = para.join(" ");
        para.clear();
        let spans = parse_inline(&joined);
        // A "paragraph" that is nothing but a page marker becomes a block-level
        // break, which is what the writer emits for a clean page boundary.
        let only_markers = spans
            .iter()
            .all(|s| matches!(s, Span::PageBreak { .. }))
            && !spans.is_empty();
        if only_markers {
            for s in spans {
                if let Span::PageBreak { label } = s {
                    blocks.push(Block::PageBreak { label });
                }
            }
            return;
        }
        let has_text = spans.iter().any(|s| match s {
            Span::Text(t) => !t.trim().is_empty(),
            Span::Emphasis(t) => !t.trim().is_empty(),
            Span::PageBreak { .. } => false,
        });
        if has_text {
            blocks.push(Block::Paragraph { spans });
        }
    };

    let mut idx = 0usize;
    while idx < lines.len() {
        let line = lines[idx];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            flush(&mut para, &mut blocks);
            idx += 1;
            continue;
        }

        // ATX heading.
        if let Some(rest) = trimmed.strip_prefix('#') {
            let mut level = 1u8;
            let mut r = rest;
            while let Some(next) = r.strip_prefix('#') {
                level += 1;
                r = next;
                if level >= 6 {
                    break;
                }
            }
            if r.starts_with(' ') || r.is_empty() {
                flush(&mut para, &mut blocks);
                blocks.push(Block::Heading {
                    level,
                    text: r.trim().trim_end_matches('#').trim().to_string(),
                });
                idx += 1;
                continue;
            }
        }

        // Setext heading: a line underlined with === or ---.
        if let Some(next) = lines.get(idx + 1) {
            let nt = next.trim();
            if !nt.is_empty() && para.is_empty() {
                if nt.chars().all(|c| c == '=') && nt.len() >= 2 {
                    blocks.push(Block::Heading {
                        level: 1,
                        text: trimmed.to_string(),
                    });
                    idx += 2;
                    continue;
                }
                if nt.chars().all(|c| c == '-') && nt.len() >= 2 && !is_separator(trimmed) {
                    blocks.push(Block::Heading {
                        level: 2,
                        text: trimmed.to_string(),
                    });
                    idx += 2;
                    continue;
                }
            }
        }

        if is_separator(trimmed) {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Separator);
            idx += 1;
            continue;
        }

        para.push(trimmed.to_string());
        idx += 1;
    }
    flush(&mut para, &mut blocks);

    Ok(Document { meta, blocks })
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\")
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_metadata() {
        let md = "---\ntitle: \"Dune: Part Two\"\nauthor: Frank Herbert\nlanguage: en\npages: 412\n---\n\nHello.\n";
        let d = parse(md).unwrap();
        assert_eq!(d.meta.title, "Dune: Part Two");
        assert_eq!(d.meta.author.as_deref(), Some("Frank Herbert"));
        assert_eq!(d.meta.language, "en");
        assert_eq!(d.meta.page_count, Some(412));
        assert_eq!(d.paragraph_count(), 1);
    }

    #[test]
    fn inline_page_marker_keeps_the_paragraph_whole() {
        let md = "---\ntitle: t\nlanguage: tr\n---\n\nTabii ki yeni resmin<!-- page: 302 --> onlari bir araya.\n";
        let d = parse(md).unwrap();
        assert_eq!(d.paragraph_count(), 1, "must not split into two paragraphs");
        assert_eq!(d.page_break_count(), 1);
        let Block::Paragraph { spans } = &d.blocks[0] else {
            panic!("expected a paragraph, got {:?}", d.blocks[0])
        };
        assert_eq!(spans.len(), 3);
        assert!(matches!(spans[1], Span::PageBreak { .. }));
    }

    #[test]
    fn standalone_marker_becomes_a_block_break() {
        let md = "---\ntitle: t\nlanguage: en\n---\n\n<!-- page: 7 -->\n\nBody text.\n";
        let d = parse(md).unwrap();
        assert!(matches!(d.blocks[0], Block::PageBreak { .. }));
        assert_eq!(d.paragraph_count(), 1);
    }

    #[test]
    fn headings_at_both_levels() {
        let md = "---\ntitle: t\nlanguage: tr\n---\n\n# 44\n\n## Carla\n\nBody.\n";
        let d = parse(md).unwrap();
        assert_eq!(d.heading_count(), 2);
        assert_eq!(d.blocks[0], Block::Heading { level: 1, text: "44".into() });
        assert_eq!(d.blocks[1], Block::Heading { level: 2, text: "Carla".into() });
    }

    #[test]
    fn wrapped_lines_join_into_one_paragraph() {
        // A human may have re-wrapped the markdown by hand.
        let md = "---\ntitle: t\nlanguage: en\n---\n\nfirst line\nsecond line\nthird line\n\nnext para\n";
        let d = parse(md).unwrap();
        assert_eq!(d.paragraph_count(), 2);
        assert!(d.plain_text().contains("first line second line third line"));
    }

    #[test]
    fn separator_is_recognised() {
        let md = "---\ntitle: t\nlanguage: en\n---\n\nA.\n\n* * *\n\nB.\n";
        let d = parse(md).unwrap();
        assert!(d.blocks.iter().any(|b| matches!(b, Block::Separator)));
    }

    #[test]
    fn emphasis_is_preserved() {
        let md = "---\ntitle: t\nlanguage: nl\n---\n\nToen *ik* op Ballings zat.\n";
        let d = parse(md).unwrap();
        let Block::Paragraph { spans } = &d.blocks[0] else { panic!() };
        assert!(spans.iter().any(|s| matches!(s, Span::Emphasis(t) if t == "ik")));
    }

    #[test]
    fn a_lone_asterisk_is_not_emphasis() {
        let md = "---\ntitle: t\nlanguage: en\n---\n\nhe said *and then stopped\n";
        let d = parse(md).unwrap();
        assert!(d.plain_text().contains("*and then stopped"));
    }

    #[test]
    fn escaped_hash_is_restored() {
        let md = "---\ntitle: t\nlanguage: en\n---\n\n\\#1 bestseller\n";
        let d = parse(md).unwrap();
        assert!(d.plain_text().starts_with("#1 bestseller"), "{:?}", d.plain_text());
    }

    #[test]
    fn missing_front_matter_is_tolerated() {
        let d = parse("Just a paragraph.\n").unwrap();
        assert_eq!(d.meta.title, "Untitled");
        assert_eq!(d.paragraph_count(), 1);
    }

    #[test]
    fn unclosed_front_matter_is_an_error() {
        let e = parse("---\ntitle: t\n\nbody\n").unwrap_err();
        assert!(matches!(e, Error::Markdown { .. }));
    }
}
