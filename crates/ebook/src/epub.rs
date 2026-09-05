//! EPUB 3 writer.
//!
//! Written by hand rather than with `epub-builder`, because the one feature
//! this project exists to get right — the `page-list` nav that records where
//! the printed pages fell — is not something that crate models.
//!
//! The container layout here was verified against calibre: `mimetype` must be
//! the first entry and stored uncompressed, then `META-INF/`, then `OEBPS/`.

use pdftomobi_core::{Block, Document, Error, PageBreakMode, Result, Span};
use std::io::Write;
use zip::write::SimpleFileOptions;

/// One output chapter: a run of blocks that becomes a single XHTML file.
struct Chapter {
    title: String,
    blocks: Vec<usize>,
}

/// A recorded page boundary: which chapter file it landed in, and its label.
struct PageAnchor {
    chapter: usize,
    id: String,
    label: String,
}

/// Split the document at level-1 headings. Front matter before the first
/// heading becomes its own chapter so nothing is lost.
fn split_chapters(doc: &Document) -> Vec<Chapter> {
    let mut chapters: Vec<Chapter> = Vec::new();
    for (i, b) in doc.blocks.iter().enumerate() {
        let start_new = matches!(b, Block::Heading { level: 1, .. });
        if start_new || chapters.is_empty() {
            let title = match b {
                Block::Heading { text, .. } => text.clone(),
                _ => {
                    if chapters.is_empty() {
                        doc.meta.title.clone()
                    } else {
                        format!("Section {}", chapters.len() + 1)
                    }
                }
            };
            chapters.push(Chapter {
                title,
                blocks: Vec::new(),
            });
        }
        chapters.last_mut().unwrap().blocks.push(i);
    }
    chapters.retain(|c| !c.blocks.is_empty());
    if chapters.is_empty() {
        chapters.push(Chapter {
            title: doc.meta.title.clone(),
            blocks: Vec::new(),
        });
    }
    chapters
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/* Headings carry the styling. Body text is left deliberately plain: no
   indent, no justification, one blank line between paragraphs. */
const CSS: &str = "\
html { font-size: 100%; }
body { margin: 0 5%; line-height: 1.5; }
h1 { font-size: 1.6em; text-align: center; margin: 2em 0 1em; page-break-before: always; }
h2 { font-size: 1.2em; text-align: center; font-weight: normal; margin: 0 0 1.5em; }
/* Never produced by the PDF path, which only emits `#` and `##`, but a
   markdown input can carry one. Styled so it does not fall back to the
   reader's default. */
h3 { font-size: 1.05em; font-weight: bold; margin: 1.5em 0 0.5em; }
p { margin: 0 0 1em; }
hr.sep { border: none; text-align: center; margin: 1.5em 0; }
hr.sep:after { content: '* * *'; }
span.pagebreak { display: none; }
span.hardbreak { display: block; page-break-after: always; }
";

fn render_chapter(doc: &Document, ch: &Chapter, ch_index: usize, mode: PageBreakMode, anchors: &mut Vec<PageAnchor>) -> String {
    let mut body = String::new();

    for &bi in &ch.blocks {
        match &doc.blocks[bi] {
            Block::Heading { level, text } => {
                let l = (*level).clamp(1, 6);
                body.push_str(&format!("<h{l}>{}</h{l}>\n", escape(text)));
            }
            Block::Paragraph { spans } => {
                body.push_str("<p>");
                for s in spans {
                    match s {
                        Span::Text(t) => body.push_str(&escape(t)),
                        Span::Emphasis(t) => {
                            body.push_str("<em>");
                            body.push_str(&escape(t));
                            body.push_str("</em>");
                        }
                        Span::PageBreak { label } => {
                            body.push_str(&page_span(ch_index, label, mode, anchors));
                        }
                    }
                }
                body.push_str("</p>\n");
            }
            Block::PageBreak { label } => {
                body.push_str(&page_span(ch_index, label, mode, anchors));
                body.push('\n');
            }
            Block::Separator => {
                body.push_str("<hr class=\"sep\" />\n");
            }
        }
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"{lang}\" lang=\"{lang}\">\n\
<head><meta charset=\"utf-8\" /><title>{title}</title>\
<link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\" /></head>\n\
<body>\n{body}</body>\n</html>\n",
        lang = escape(&doc.meta.language),
        title = escape(&ch.title),
        body = body
    )
}

fn page_span(ch_index: usize, label: &str, mode: PageBreakMode, anchors: &mut Vec<PageAnchor>) -> String {
    match mode {
        PageBreakMode::None => String::new(),
        PageBreakMode::Anchors => {
            let id = format!("page_{}", sanitise_id(label));
            // Guard against a duplicate printed label, which happens when a
            // scan repeats a number or the footer was misread.
            if anchors.iter().any(|a| a.id == id) {
                return String::new();
            }
            anchors.push(PageAnchor {
                chapter: ch_index,
                id: id.clone(),
                label: label.to_string(),
            });
            format!(
                "<span class=\"pagebreak\" id=\"{id}\" epub:type=\"pagebreak\" role=\"doc-pagebreak\" aria-label=\"{}\"></span>",
                escape(label)
            )
        }
        PageBreakMode::Hard => {
            let id = format!("page_{}", sanitise_id(label));
            if anchors.iter().any(|a| a.id == id) {
                return String::new();
            }
            anchors.push(PageAnchor {
                chapter: ch_index,
                id: id.clone(),
                label: label.to_string(),
            });
            // A block-level element that forces a real break. Survives
            // conversion to MOBI, at the cost of fighting reflow.
            format!("<span class=\"hardbreak\" id=\"{id}\" epub:type=\"pagebreak\" role=\"doc-pagebreak\" aria-label=\"{}\"></span>", escape(label))
        }
    }
}

fn sanitise_id(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    if t.chars().next().map(|c| c.is_numeric()).unwrap_or(true) {
        format!("n{t}")
    } else {
        t
    }
}

pub struct EpubOptions {
    pub page_breaks: PageBreakMode,
}

/// Write `doc` as an EPUB 3 file.
pub fn write(doc: &Document, path: &std::path::Path, opts: &EpubOptions) -> Result<()> {
    let chapters = split_chapters(doc);
    let mut anchors: Vec<PageAnchor> = Vec::new();
    let mut chapter_html: Vec<String> = Vec::with_capacity(chapters.len());
    for (i, ch) in chapters.iter().enumerate() {
        chapter_html.push(render_chapter(doc, ch, i, opts.page_breaks, &mut anchors));
    }

    let uid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let modified = "2026-09-04T00:00:00Z".to_string();

    // ---- content.opf
    let mut manifest = String::new();
    manifest.push_str("<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n");
    manifest.push_str("<item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n");
    let mut spine = String::new();
    for i in 0..chapters.len() {
        manifest.push_str(&format!(
            "<item id=\"c{i}\" href=\"ch{i}.xhtml\" media-type=\"application/xhtml+xml\"/>\n"
        ));
        spine.push_str(&format!("<itemref idref=\"c{i}\"/>\n"));
    }
    let author_meta = match &doc.meta.author {
        Some(a) => format!("<dc:creator>{}</dc:creator>\n", escape(a)),
        None => String::new(),
    };
    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"bookid\" xml:lang=\"{lang}\">\n\
<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
<dc:identifier id=\"bookid\">{uid}</dc:identifier>\n\
<dc:title>{title}</dc:title>\n\
{author}\
<dc:language>{lang}</dc:language>\n\
<meta property=\"dcterms:modified\">{modified}</meta>\n\
</metadata>\n<manifest>\n{manifest}</manifest>\n<spine>\n{spine}</spine>\n</package>\n",
        lang = escape(&doc.meta.language),
        uid = uid,
        title = escape(&doc.meta.title),
        author = author_meta,
        modified = modified,
        manifest = manifest,
        spine = spine
    );

    // ---- nav.xhtml, with both a toc and a page-list
    let mut toc = String::new();
    for (i, ch) in chapters.iter().enumerate() {
        let label = if ch.title.trim().is_empty() {
            format!("Section {}", i + 1)
        } else {
            ch.title.clone()
        };
        toc.push_str(&format!(
            "<li><a href=\"ch{i}.xhtml\">{}</a></li>\n",
            escape(&label)
        ));
    }
    let page_list = if anchors.is_empty() {
        String::new()
    } else {
        let mut items = String::new();
        for a in &anchors {
            items.push_str(&format!(
                "<li><a href=\"ch{}.xhtml#{}\">{}</a></li>\n",
                a.chapter,
                a.id,
                escape(&a.label)
            ));
        }
        format!(
            "<nav epub:type=\"page-list\" hidden=\"hidden\"><h2>Pages</h2><ol>\n{items}</ol></nav>\n"
        )
    };
    let nav = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"{lang}\" lang=\"{lang}\">\n\
<head><meta charset=\"utf-8\" /><title>Contents</title></head>\n<body>\n\
<nav epub:type=\"toc\" id=\"toc\"><h1>Contents</h1><ol>\n{toc}</ol></nav>\n{page_list}</body>\n</html>\n",
        lang = escape(&doc.meta.language),
        toc = toc,
        page_list = page_list
    );

    // ---- zip it
    let file = std::fs::File::create(path).map_err(|e| Error::io(path, e))?;
    let mut zip = zip::ZipWriter::new(file);

    // `mimetype` first, stored, no extra fields. This is what makes the file a
    // recognisable EPUB.
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("mimetype", stored)
        .map_err(zip_err(path))?;
    zip.write_all(b"application/epub+zip")
        .map_err(|e| Error::io(path, e))?;

    let deflate = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut add = |name: &str, data: &str| -> Result<()> {
        zip.start_file(name, deflate).map_err(zip_err(path))?;
        zip.write_all(data.as_bytes())
            .map_err(|e| Error::io(path, e))?;
        Ok(())
    };

    add(
        "META-INF/container.xml",
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n\
<rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles>\n\
</container>\n",
    )?;
    add("OEBPS/content.opf", &opf)?;
    add("OEBPS/nav.xhtml", &nav)?;
    add("OEBPS/style.css", CSS)?;
    for (i, html) in chapter_html.iter().enumerate() {
        add(&format!("OEBPS/ch{i}.xhtml"), html)?;
    }

    zip.finish().map_err(zip_err(path))?;
    Ok(())
}

fn zip_err(path: &std::path::Path) -> impl Fn(zip::result::ZipError) -> Error + '_ {
    move |e| Error::Other(anyhow::anyhow!("writing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdftomobi_core::Meta;

    fn doc() -> Document {
        Document {
            meta: Meta {
                title: "Kocamin Karisi".into(),
                author: Some("Jane Corry".into()),
                language: "tr".into(),
                ..Default::default()
            },
            blocks: vec![
                Block::Paragraph { spans: vec![Span::text("Front matter.")] },
                Block::Heading { level: 1, text: "44".into() },
                Block::Heading { level: 2, text: "Carla".into() },
                Block::Paragraph {
                    spans: vec![
                        Span::text("Tabii ki"),
                        Span::PageBreak { label: "301".into() },
                        Span::text(" onlari."),
                    ],
                },
                Block::Heading { level: 1, text: "45".into() },
                Block::Paragraph { spans: vec![Span::text("Next chapter.")] },
            ],
        }
    }

    #[test]
    fn splits_on_level_one_headings_and_keeps_front_matter() {
        let ch = split_chapters(&doc());
        // front matter, chapter 44, chapter 45
        assert_eq!(ch.len(), 3, "{:?}", ch.iter().map(|c| &c.title).collect::<Vec<_>>());
        assert_eq!(ch[1].title, "44");
        assert_eq!(ch[2].title, "45");
    }

    #[test]
    fn writes_a_zip_with_mimetype_first_and_uncompressed() {
        let dir = std::env::temp_dir().join(format!("pdftomobi-epub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.epub");
        write(&doc(), &p, &EpubOptions { page_breaks: PageBreakMode::Anchors }).unwrap();

        let bytes = std::fs::read(&p).unwrap();
        // The reader identifies an EPUB by finding this at a fixed offset.
        assert_eq!(&bytes[30..38], b"mimetype");
        assert_eq!(&bytes[38..58], b"application/epub+zip");

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&p).unwrap()).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        assert_eq!(names[0], "mimetype");
        assert!(names.contains(&"META-INF/container.xml".to_string()));
        assert!(names.contains(&"OEBPS/content.opf".to_string()));
        assert!(names.contains(&"OEBPS/nav.xhtml".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn anchors_mode_emits_a_page_list() {
        let mut anchors = Vec::new();
        let d = doc();
        let chs = split_chapters(&d);
        for (i, c) in chs.iter().enumerate() {
            render_chapter(&d, c, i, PageBreakMode::Anchors, &mut anchors);
        }
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].label, "301");
        assert_eq!(anchors[0].id, "page_n301");
    }

    #[test]
    fn none_mode_emits_no_anchors() {
        let mut anchors = Vec::new();
        let d = doc();
        let chs = split_chapters(&d);
        for (i, c) in chs.iter().enumerate() {
            let html = render_chapter(&d, c, i, PageBreakMode::None, &mut anchors);
            assert!(!html.contains("pagebreak"));
        }
        assert!(anchors.is_empty());
    }

    #[test]
    fn hard_mode_uses_a_forcing_class() {
        let mut anchors = Vec::new();
        let d = doc();
        let chs = split_chapters(&d);
        let html: String = chs
            .iter()
            .enumerate()
            .map(|(i, c)| render_chapter(&d, c, i, PageBreakMode::Hard, &mut anchors))
            .collect();
        assert!(html.contains("hardbreak"));
    }

    #[test]
    fn duplicate_page_labels_do_not_produce_duplicate_ids() {
        let d = Document {
            meta: Meta { title: "t".into(), language: "en".into(), ..Default::default() },
            blocks: vec![
                Block::PageBreak { label: "7".into() },
                Block::Paragraph { spans: vec![Span::text("a")] },
                Block::PageBreak { label: "7".into() },
                Block::Paragraph { spans: vec![Span::text("b")] },
            ],
        };
        let mut anchors = Vec::new();
        let chs = split_chapters(&d);
        for (i, c) in chs.iter().enumerate() {
            render_chapter(&d, c, i, PageBreakMode::Anchors, &mut anchors);
        }
        assert_eq!(anchors.len(), 1, "the repeat must be dropped, not duplicated");
    }

    #[test]
    fn xml_special_characters_are_escaped() {
        assert_eq!(escape("a & b < c"), "a &amp; b &lt; c");
    }

    #[test]
    fn body_text_carries_no_styling_and_paragraphs_are_separated_by_a_blank_line() {
        assert!(CSS.contains("p { margin: 0 0 1em; }"), "paragraphs need a gap, not an indent");
        assert!(!CSS.contains("text-indent"), "body text must not be indented");
        assert!(!CSS.contains("justify"), "body text must stay ragged right");

        let mut anchors = Vec::new();
        let d = doc();
        let chs = split_chapters(&d);
        let html: String = chs
            .iter()
            .enumerate()
            .map(|(i, c)| render_chapter(&d, c, i, PageBreakMode::Anchors, &mut anchors))
            .collect();
        // Every paragraph is a bare <p>; there is no first-paragraph special case
        // left to carry a class.
        assert!(html.contains("<p>"));
        assert!(!html.contains("<p class="), "paragraphs must not be classed");
    }

    #[test]
    fn every_heading_level_the_parser_can_emit_is_styled() {
        // The PDF path only produces `#` and `##`, but a markdown input can
        // carry a `###`, and an unstyled h3 falls back to the reader's default.
        for h in ["h1 {", "h2 {", "h3 {"] {
            assert!(CSS.contains(h), "{h} is unstyled");
        }
    }
}
