//! The document model. This is what layer 3 produces and layer 4 consumes,
//! and it round-trips through markdown without loss.

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub title: String,
    pub author: Option<String>,
    /// BCP-47, for EPUB `dc:language`.
    pub language: String,
    /// Original filename, for provenance.
    pub source: Option<String>,
    pub page_count: Option<usize>,
    pub generator: Option<String>,
}

/// A run of inline content inside a paragraph.
///
/// `PageBreak` is a span rather than only a block because printed pages usually
/// break *mid-paragraph*. Recording it inline is what lets us keep the
/// paragraph whole while still knowing where the page boundary fell.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Span {
    Text(String),
    Emphasis(String),
    PageBreak { label: String },
}

impl Span {
    pub fn text(s: impl Into<String>) -> Self {
        Span::Text(s.into())
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Block {
    /// `level` 1 opens a new chapter (a new XHTML file in the EPUB).
    Heading { level: u8, text: String },
    Paragraph { spans: Vec<Span> },
    /// A page boundary that fell cleanly between two paragraphs.
    PageBreak { label: String },
    /// A scene divider (`* * *`, `---`).
    Separator,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Document {
    pub meta: Meta,
    pub blocks: Vec<Block>,
}

impl Document {
    /// Plain concatenated text, for diffing and for accuracy scoring against a
    /// ground-truth file.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            match b {
                Block::Heading { text, .. } => {
                    out.push_str(text);
                    out.push_str("\n\n");
                }
                Block::Paragraph { spans } => {
                    for s in spans {
                        match s {
                            Span::Text(t) => out.push_str(t),
                            Span::Emphasis(t) => out.push_str(t),
                            Span::PageBreak { .. } => {}
                        }
                    }
                    out.push_str("\n\n");
                }
                Block::PageBreak { .. } => {}
                Block::Separator => out.push_str("* * *\n\n"),
            }
        }
        out
    }

    pub fn paragraph_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|b| matches!(b, Block::Paragraph { .. }))
            .count()
    }

    pub fn heading_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|b| matches!(b, Block::Heading { .. }))
            .count()
    }

    pub fn page_break_count(&self) -> usize {
        self.blocks
            .iter()
            .map(|b| match b {
                Block::PageBreak { .. } => 1,
                Block::Paragraph { spans } => spans
                    .iter()
                    .filter(|s| matches!(s, Span::PageBreak { .. }))
                    .count(),
                _ => 0,
            })
            .sum()
    }

    pub fn char_count(&self) -> usize {
        self.plain_text().chars().count()
    }
}
