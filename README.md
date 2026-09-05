# pdf-to-ebook

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
./target/release/pdf-to-ebook-gui

# CLI, from a PDF
./target/release/pdf-to-ebook book.pdf --lang tur --format md,epub
./target/release/pdf-to-ebook book.pdf --lang eng --format epub,mobi --llm suspicious

# proofread on two machines at once: the batches go to whichever is free first
./target/release/pdf-to-ebook book.pdf --llm always \
  --ollama-url http://desktop:11434,http://laptop:11434
./target/release/pdf-to-ebook book.pdf --pages 40-45 --format md     # quick look

# CLI, from markdown — your own, or one a previous run wrote and you fixed
./target/release/pdf-to-ebook notes.md --format epub
./target/release/pdf-to-ebook book.md --format mobi,azw3 --title "A Title" --author "A Name"

./target/release/pdf-to-ebook --help
```

Markdown is the interface between extraction and ebook building, so a markdown
input skips straight to the second half: no OCR, no model, no page analysis, and
the options that drive those do nothing (the CLI says so rather than pretending).
`--title` and `--author` override the front matter; `--lang` only fills in a
language the front matter does not already give.

Every run also writes a `*.report.md` listing what was removed as headers or
footers, every correction the model made or refused, and any warnings.

## Configuration

Two extension points, both files, neither needing a rebuild.

### `.env` — where things are on this machine

Copy `.env.example` to `.env` and edit. It holds only what is about the
machine: which ollama servers, which model, where `tesseract` and calibre live,
the default language and DPI. Nothing in it is a secret — the whole pipeline is
local and there is no API key anywhere in it.

```sh
cp .env.example .env
```

| Setting | Default | For |
|---|---|---|
| `PDF_TO_EBOOK_OLLAMA_URL` | `http://localhost:11434` | one server, or a comma-separated list. Falls back to `OLLAMA_HOST` |
| `PDF_TO_EBOOK_LLM_MODEL` | `gemma4:e4b` | the proofreading model |
| `PDF_TO_EBOOK_OCR_LANG` | `eng` | default `--lang`, a tesseract code |
| `PDF_TO_EBOOK_DPI` | `300` | default `--dpi` for the OCR path |
| `PDF_TO_EBOOK_PROMPT_DIR` | built-in | load the prompt from a directory instead |
| `PDF_TO_EBOOK_CACHE_DIR` | OS cache dir | where model replies are cached |
| `TESSERACT_BIN` | on `PATH` | only when tesseract is somewhere unusual |
| `EBOOK_CONVERT` | discovered | only when calibre is somewhere unusual |

A command-line flag beats these, and a real environment variable beats the
file. `.env` is found at or above the working directory, so one at the repo
root covers `cargo run` from any crate; `PDF_TO_EBOOK_ENV_FILE` names one
outright.

### `prompts/` — what the model is asked

The proofreading prompt is a file, because the prompt is the single largest
lever on proofreading quality and re-tuning it should be a diff you can read.

```
prompts/proofread.md          the task, rule 0, the fault classes, the limits
prompts/languages/tur.md      what is only true of Turkish
```

`prompts/proofread.md` holds what is true of any Latin-script scan.
`languages/<code>.md` is named after the tesseract code `--lang` takes, so
adding a language is adding a file — see `prompts/languages/README.md`.

Both are compiled into the binary, so an installed `pdf-to-ebook` needs no
`prompts/` beside it. `PDF_TO_EBOOK_PROMPT_DIR` overrides them **per file**: a
directory holding only `languages/nld.md` adds a Dutch pack and keeps the
built-in general prompt.

The reply cache is keyed on the rendered prompt, so editing one of these
invalidates it and the next run pays full price — a cached answer to a
different question is worse than a slow run. A run using an overridden prompt
says `prompt loaded from …`, because every number below is a number about a
particular prompt.

## Requirements

Only as needed:

| For | Needs |
|---|---|
| Markdown → EPUB | nothing beyond Rust |
| A PDF with a text layer → markdown / EPUB | nothing beyond Rust |
| Pages with no text layer (OCR) | `tesseract` + language data (`brew install tesseract tesseract-lang`) |
| MOBI / AZW3 | `calibre` (`brew install --cask calibre`) |
| Typo proofreading | `ollama` running a local model (default `gemma4:e4b`) — one server, or several |

pdfium is downloaded and cached automatically on first run. Set `TESSERACT_BIN`
or `EBOOK_CONVERT` in `.env` if either tool is installed somewhere unusual.

## How good is it

| Check | Result |
|---|---|
| OCR accuracy against known-good text | 99.26% word, 99.86% character |
| A 419-page scanned Turkish novel against pdfium's own text | 99.17% word |
| That book: text, layout, chapters, EPUB | 6.7s |
| Tests | 143 passing |

## Docs

- [`docs/architecture.md`](docs/architecture.md) — the four layers, the markdown
  contract (and why it makes markdown an input too), every pipeline stage
- [`docs/findings.md`](docs/findings.md) — measured evidence, the traps that
  cost real time, and known limitations
- [`test/README.md`](test/README.md) — the seven fixtures and what each exercises

## Support

The tool is free and always will be. If it saved you an afternoon, you can
[buy me a coffee](https://buymeacoffee.com/deniztasdogen).

## Licence

Public domain, under [The Unlicense](UNLICENSE) — do whatever you like with it,
no attribution needed.
