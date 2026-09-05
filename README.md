# pdfToMobi

Build an EPUB or Kindle book from a PDF or from a markdown file. Keeps
paragraphs and printed page breaks; ignores images.

Written in Rust, in four layers: input (GUI or CLI) → orchestration →
extraction (text layer, OCR, local-LLM proofreading) → ebook building. The
boundary between the last two is a **markdown file on disk**, so a bad page can
be corrected by hand and the ebook rebuilt without re-running OCR.

## Build

```sh
cargo build --release
```

## Use

```sh
# GUI: choose a PDF, a language and the output formats
./target/release/pdftomobi-gui

# CLI, from a PDF
./target/release/pdftomobi book.pdf --lang tur --format md,epub
./target/release/pdftomobi book.pdf --lang eng --format epub,mobi --llm suspicious

# proofread on two machines at once: the batches go to whichever is free first
./target/release/pdftomobi book.pdf --llm always \
  --ollama-url http://desktop:11434,http://laptop:11434
./target/release/pdftomobi book.pdf --pages 40-45 --format md     # quick look

# CLI, from markdown — your own, or one a previous run wrote and you fixed
./target/release/pdftomobi notes.md --format epub
./target/release/pdftomobi book.md --format mobi,azw3 --title "A Title" --author "A Name"

./target/release/pdftomobi --help
```

Markdown is the interface between extraction and ebook building, so a markdown
input skips straight to the second half: no OCR, no model, no page analysis, and
the options that drive those do nothing (the CLI says so rather than pretending).
`--title` and `--author` override the front matter; `--lang` only fills in a
language the front matter does not already give.

Every run also writes a `*.report.md` listing what was removed as headers or
footers, every correction the model made or refused, and any warnings.

## Requirements

Only as needed:

| For | Needs |
|---|---|
| Markdown → EPUB | nothing beyond Rust |
| A PDF with a text layer → markdown / EPUB | nothing beyond Rust |
| Pages with no text layer (OCR) | `tesseract` + language data (`brew install tesseract tesseract-lang`) |
| MOBI / AZW3 | `calibre` (`brew install --cask calibre`) |
| Typo proofreading | `ollama` running a local model (default `gemma4:e4b`) — one server, or several |

pdfium is downloaded and cached automatically on first run.

## How good is it

| Check | Result |
|---|---|
| OCR accuracy against known-good text | 99.26% word, 99.86% character |
| A 419-page scanned Turkish novel against pdfium's own text | 99.17% word |
| That book: text, layout, chapters, EPUB | 6.7s |
| Tests | 135 passing |

## Docs

- [`docs/architecture.md`](docs/architecture.md) — the four layers, the markdown
  contract (and why it makes markdown an input too), every pipeline stage
- [`docs/findings.md`](docs/findings.md) — measured evidence, the traps that
  cost real time, and known limitations
- [`test/README.md`](test/README.md) — the seven fixtures and what each exercises
