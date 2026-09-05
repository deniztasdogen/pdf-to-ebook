# pdfToMobi docs

PDF → EPUB / Kindle text extractor. Rust, four layers.

| Doc | What it holds |
|---|---|
| [`architecture.md`](architecture.md) | The four layers as built, the markdown files that are the contract between them, and every pipeline stage |
| [`findings.md`](findings.md) | Evidence. Sections 1-6 from the initial investigation, section 7 from running the code over the whole corpus, section 8 measured quality, section 9 known limitations |
| [`plan.md`](plan.md) | The original plan, kept for the record, with a status note on what changed |
| [`../test/README.md`](../test/README.md) | The eight test fixtures and what each one exercises |

## Quick start

```sh
cargo build --release

# GUI: pick a file, a language and the formats
./target/release/pdftomobi-gui

# CLI
./target/release/pdftomobi book.pdf --lang tur --format md,epub
./target/release/pdftomobi book.pdf --lang eng --format epub,mobi --llm suspicious
./target/release/pdftomobi book.pdf --pages 40-45 --format md    # fast iteration

# Proofread a markdown file from an earlier run, without touching the PDF again
./target/release/pdftomobi out/book.md --llm always --format epub
```

Needs `tesseract` (+ language data) only for pages without a text layer,
`calibre` only for MOBI/AZW3, and `ollama` only for proofreading. A born-digital
PDF to EPUB needs none of them.

## Headline answers

**Rust was enough — no hand-written C++.** pdfium (text geometry *and* page
rasterisation) and tesseract are consumed as prebuilt libraries. The only
external tool is calibre's `ebook-convert` for MOBI packaging, because no usable
native Rust MOBI writer exists.

**Markdown is a real file, not an in-memory handoff.** Layer 2 writes it,
everything after it reads it back off disk. That makes it the actual contract: a
bad OCR page can be fixed in a text editor and the EPUB rebuilt without
re-running OCR. Proofreading sits between two of these files — `book.md` is what
the PDF said, `book.proofread.md` is what the model made of it, and the EPUB is
built from the second one — so `diff` is the whole audit of the model's work.

**Page breaks are joined, not split.** A paragraph running across a page
boundary stays one paragraph, with an EPUB 3 `pagebreak` anchor inserted at the
join. Splitting at every boundary would add 419 false paragraph breaks to the
Turkish book. Verified caveat: calibre's MOBI writer discards these anchors, so
EPUB is the format that preserves page breaks.

**The LLM pass is independent of the OCR decision.** The Turkish book's text
layer is *itself* OCR output (Acrobat ClearScan), so it carries OCR typos even
though the pipeline never runs OCR on it.

## Measured quality

| Check | Result |
|---|---|
| OCR accuracy against known-good text | **99.26% word**, 99.86% character |
| Turkish book against pdfium's own text | **99.17% word** |
| 419-page book: text, layout, EPUB | **6.7s** |
| Tests | **113 passing** |

## Traps that cost real time

All verified, all with a regression test. Details in `findings.md`.

- Use pdfium's `loose_bounds()`, never `tight_bounds()`. The ink box shatters
  every line containing a descender or a Turkish diacritic.
- ClearScan reports **one box for a whole glyph cluster** — a 58pt-wide `k`.
  Measure a word's backward jump from the word's start, not the previous glyph's
  end, or `Rahatlıyorum` becomes `Ra hatlıyorum` across the whole book.
- Let the PDF's own space characters decide word breaks. Geometry only as a
  per-page fallback, for scans that lost their spaces.
- A real space can be reported with font size 1.0. Recognise whitespace *before*
  filtering on size.
- Normalise vertical bands by font size, and cap by the page median: a 42pt box
  on 14pt text merges two lines, and a 23.75pt box rule chains a whole page.
- Page sizes drift from 474×788 to 545×866pt *within one document*, so margins
  and indent thresholds are per page, never constants.
- Split columns **before** assembling lines, or two columns interleave.
- Size a column gutter in ems, not page fractions: a real CVPR gutter is under
  4% of the body width.
- Column count is a document property. Per-page detection alone gave one page
  four columns.
- Match running headers fuzzily — the same header is read four different ways.
- A nanosecond timestamp is not a unique filename. Parallel OCR silently
  duplicated one page and lost another.
- Font size is useless for chapter detection here: one chapter heading is
  *smaller* than its body text. Weight centring and isolation instead.
- Non-fiction section headings are bold, not big.
- `ocrs`, the pure-Rust OCR crate, is disqualified: its alphabet is hardcoded
  ASCII, with no `ı ğ ş ç` or `ë ï`.
