//! **Layer 4: ebook building.**
//!
//! Markdown in, EPUB/MOBI out. This layer knows nothing about PDFs, OCR or
//! language models — it only reads the markdown layer 3 wrote, which means the
//! markdown can be corrected by hand in between.

pub mod epub;
pub mod md_parse;
pub mod mobi;

use pdftomobi_core::{Document, Error, OutputFormat, PageBreakMode, Result};
use std::path::Path;

pub use md_parse::parse;

/// Read a markdown file written by layer 3.
pub fn read_markdown(path: &Path) -> Result<Document> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    md_parse::parse(&text)
}

/// Build an EPUB from a markdown file.
pub fn epub_from_markdown(md: &Path, out: &Path, page_breaks: PageBreakMode) -> Result<Document> {
    let doc = read_markdown(md)?;
    epub::write(&doc, out, &epub::EpubOptions { page_breaks })?;
    Ok(doc)
}

/// Build a Kindle format from an existing EPUB.
pub fn kindle_from_epub(epub_path: &Path, out: &Path, format: OutputFormat) -> Result<()> {
    mobi::convert(epub_path, out, format)
}
