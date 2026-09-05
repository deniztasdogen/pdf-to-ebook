//! Layout analysis: positioned words in, a structured document out.
//!
//! Both input paths (PDF text layer and OCR) feed the same code here, which is
//! the whole point of the intermediate representation.

pub mod chrome;
pub mod columns;
pub mod headings;
pub mod lines;
pub mod paragraphs;

use crate::model::{
    Column, DropReason, DroppedLine, Line, PageLayout, PageSource, Word,
};
use headings::HeadingKind;
use paragraphs::RawParagraph;
use pdf_to_ebook_core::{Block, Crop, Document, Meta, PageBreakMode, Span};

/// A page must hold at least this many lines of text before a paragraph is
/// allowed to run off it onto the next page.
const MIN_LINES_TO_CONTINUE: usize = 5;

/// One analysed item on a page, before cross-page assembly.
#[derive(Debug, Clone)]
enum Item {
    Heading { level: u8, text: String },
    Para(RawParagraph),
}

/// Discard words inside the cropped margins.
pub fn apply_crop(words: Vec<Word>, w: f32, h: f32, crop: Crop) -> (Vec<Word>, usize) {
    if crop.is_zero() {
        return (words, 0);
    }
    let x0 = w * crop.left;
    let x1 = w * (1.0 - crop.right);
    let y0 = h * crop.top;
    let y1 = h * (1.0 - crop.bottom);
    let before = words.len();
    let kept: Vec<Word> = words
        .into_iter()
        .filter(|word| {
            let c = word.bbox;
            c.center_x() >= x0 && c.center_x() <= x1 && c.y0 >= y0 && c.y1 <= y1
        })
        .collect();
    let removed = before - kept.len();
    (kept, removed)
}

/// Coarse full-page lines, used only to spot chrome. Not the reading-order
/// lines — those are built per column after the body is isolated.
pub fn coarse_lines(words: &[Word]) -> Vec<Line> {
    lines::assemble(words.to_vec())
}

/// Turn one page's words into a [`PageLayout`].
pub fn analyse_page(
    index: usize,
    source: PageSource,
    words: Vec<Word>,
    width_pt: f32,
    height_pt: f32,
    repeated: &[String],
    single_page: bool,
    crop: Crop,
    plan: Option<&columns::ColumnPlan>,
) -> PageLayout {
    let (words, cropped) = apply_crop(words, width_pt, height_pt, crop);
    let mut dropped: Vec<DroppedLine> = Vec::new();
    if cropped > 0 {
        dropped.push(DroppedLine {
            text: format!("{cropped} word(s) inside the cropped margins"),
            reason: DropReason::Cropped,
        });
    }

    // Settle the column gutters first, from every word on the page. They are
    // needed twice: to recognise furniture that crosses a gutter, and to split
    // the body afterwards. Deriving them once keeps the two consistent.
    let mut gutters = columns::candidate_splits(&words);
    if let Some(plan) = plan {
        let (bx0, bx1) = columns::body_extent(&words);
        gutters = columns::reconcile(gutters, bx0, bx1, plan);
    }

    // Chrome is identified on coarse full-page lines, because a running header
    // spans the whole page above whatever column structure follows.
    let coarse = coarse_lines(&words);
    let stripped = chrome::strip(
        coarse,
        width_pt,
        height_pt,
        repeated,
        single_page,
        &gutters,
    );
    dropped.extend(stripped.dropped);

    // Body words are those belonging to lines that survived.
    let body_words: Vec<Word> = stripped
        .body
        .into_iter()
        .flat_map(|l| l.words.into_iter())
        .collect();

    // Columns first, then lines within each column. Doing it the other way
    // round interleaves the two columns of a spread.
    let groups = columns::split_at(body_words, &gutters);
    let mut cols: Vec<Column> = Vec::new();
    for g in groups {
        if g.is_empty() {
            continue;
        }
        let ls = lines::assemble(g);
        if ls.is_empty() {
            continue;
        }
        let bbox = match crate::geom::Rect::union_all(ls.iter().map(|l| &l.bbox)) {
            Some(b) => b,
            None => continue,
        };
        cols.push(Column { lines: ls, bbox });
    }
    // Left-to-right reading order.
    cols.sort_by(|a, b| a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap());

    PageLayout {
        index,
        source,
        width_pt,
        height_pt,
        columns: cols,
        printed_label: stripped.printed_label,
        dropped,
    }
}

/// Per-page paragraphs and headings, in reading order.
fn page_items(page: &PageLayout) -> Vec<Item> {
    let mut items = Vec::new();
    for col in &page.columns {
        let stats = paragraphs::column_stats(&col.lines);
        let center = col.bbox.center_x();
        let paras = paragraphs::group(&col.lines, page.index, stats);

        for (i, p) in paras.iter().enumerate() {
            // Only a single-line paragraph can be a heading.
            if p.lines.len() == 1 {
                let gap_before = if i == 0 {
                    None
                } else {
                    paras[i - 1]
                        .lines
                        .last()
                        .map(|prev| p.lines[0].bbox.y0 - prev.bbox.y0)
                };
                let gap_after = paras
                    .get(i + 1)
                    .and_then(|n| n.lines.first())
                    .map(|next| next.bbox.y0 - p.lines[0].bbox.y0);
                if let Some(kind) =
                    headings::classify(&p.lines[0], &stats, center, gap_before, gap_after)
                {
                    let level = match kind {
                        HeadingKind::Chapter => 1,
                        HeadingKind::Subtitle => 2,
                    };
                    items.push(Item::Heading {
                        level,
                        text: p.text.clone(),
                    });
                    continue;
                }
            }
            items.push(Item::Para(p.clone()));
        }
    }
    items
}

pub struct Assembled {
    pub document: Document,
    pub ambiguous_joins: Vec<String>,
}

/// Stitch pages into a document, joining paragraphs that run across page
/// boundaries and recording where those boundaries fell.
pub fn assemble(pages: &[PageLayout], meta: Meta, mode: PageBreakMode) -> Assembled {
    let mut blocks: Vec<Block> = Vec::new();
    let mut ambiguous: Vec<String> = Vec::new();
    // Index of the block holding the paragraph most recently emitted, so the
    // next page can append to it.
    let mut open_para: Option<usize> = None;
    let mut open_ends_sentence = true;
    let mut open_hyphenated = false;
    // How many lines the previous page held. A title page, a part divider or a
    // near-blank page must not have its last line welded to the next page's
    // first paragraph, which is what turned "By Jane Austen" into the opening
    // of the copyright notice.
    let mut prev_page_lines = 0usize;

    for page in pages {
        let items = page_items(page);
        let this_page_lines = page.all_lines().count();
        let label = page.label();
        let mut pending_marker = !matches!(mode, PageBreakMode::None);

        for (i, item) in items.iter().enumerate() {
            match item {
                Item::Heading { level, text } => {
                    if pending_marker {
                        blocks.push(Block::PageBreak {
                            label: label.clone(),
                        });
                        pending_marker = false;
                    }
                    blocks.push(Block::Heading {
                        level: *level,
                        text: text.clone(),
                    });
                    open_para = None;
                    open_ends_sentence = true;
                    open_hyphenated = false;
                }
                Item::Para(p) => {
                    ambiguous.extend(p.ambiguous_joins.iter().cloned());
                    // A paragraph continues across the page boundary when the
                    // previous page stopped mid-sentence and this is the first
                    // item on the page and it is not indented.
                    let continues = i == 0
                        && pending_marker
                        && open_para.is_some()
                        && !open_ends_sentence
                        && !p.starts_indented
                        && prev_page_lines >= MIN_LINES_TO_CONTINUE;

                    if continues {
                        let idx = open_para.unwrap();
                        if let Block::Paragraph { spans } = &mut blocks[idx] {
                            // The marker goes *inside* the paragraph, at the
                            // exact join, so the page boundary is recorded
                            // without breaking the paragraph in two.
                            if !matches!(mode, PageBreakMode::None) {
                                spans.push(Span::PageBreak {
                                    label: label.clone(),
                                });
                            }
                            let joiner = if open_hyphenated { "" } else { " " };
                            spans.push(Span::Text(format!("{joiner}{}", p.text)));
                        }
                        pending_marker = false;
                        open_ends_sentence = p.ends_with_sentence_end();
                        open_hyphenated = p.ends_hyphenated;
                        continue;
                    }

                    if pending_marker {
                        blocks.push(Block::PageBreak {
                            label: label.clone(),
                        });
                        pending_marker = false;
                    }
                    blocks.push(Block::Paragraph {
                        spans: vec![Span::Text(p.text.clone())],
                    });
                    open_para = Some(blocks.len() - 1);
                    open_ends_sentence = p.ends_with_sentence_end();
                    open_hyphenated = p.ends_hyphenated;
                }
            }
        }

        // A page with no content at all still gets its marker, so page
        // numbering stays aligned with the source.
        if pending_marker {
            blocks.push(Block::PageBreak {
                label: label.clone(),
            });
        }
        prev_page_lines = this_page_lines;
    }

    Assembled {
        document: Document { meta, blocks },
        ambiguous_joins: ambiguous,
    }
}
