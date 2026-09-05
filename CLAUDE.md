# pdfToMobi — working notes

Rust workspace. Builds an EPUB or Kindle book from a **PDF** or from a
**markdown file**. Keeps paragraphs and printed page breaks; ignores images.

Read [`docs/architecture.md`](docs/architecture.md) before changing pipeline
behaviour, and [`docs/findings.md`](docs/findings.md) before "fixing" anything
in layout analysis — most of what looks arbitrary there is a measured fix for a
specific document, with a regression test attached.

## Commands

Run from the repo root.

```sh
cargo build --release           # binaries land in target/release/
cargo test --workspace          # 135 tests, all should pass
cargo clippy --workspace --all-targets
```

There is **no CI and no formatter gate** here, and the git repo has no commits
yet, so there is no history to bisect either. `cargo test --workspace` is the
whole safety net, so run it before claiming anything works.

## Two hard rules

**Do not run `cargo fmt`.** The codebase is hand-formatted and is *not*
rustfmt-clean — `cargo fmt --check` reports diffs in 19 of the 29 source files,
and they are deliberate (the `LANGUAGES` table in `crates/core/src/config.rs` is
aligned by hand, long `writeln!` chains are broken for reading). Running it
would rewrite most of the repo. Match the surrounding style by hand instead.

**Clippy has pre-existing warnings** in `crates/extract` and `crates/ebook`.
Leave them alone. Only make sure code *you* touch is clean.

## The layers

Each is a crate and depends only on the layer below it plus `core`.

| Crate | Layer | Owns |
|---|---|---|
| `crates/core` | 0 | `Config`, `Document`/`Block`/`Span`, `Error`, progress events. **No PDF, OCR, HTTP or UI code** — that is what lets the GUI depend on it without pulling in pdfium. |
| `crates/cli`, `crates/gui` | 1 | Build a `Config`, hand it to layer 2. No conversion logic. |
| `crates/orchestrator` | 2 | Order of operations, and nothing else. Dispatches on input kind, writes the report. |
| `crates/extract` | 3 | PDF → markdown: text layer, OCR, layout analysis, LLM proofreading. Knows nothing about EPUB. The proofreading pass takes a `Document`, so layer 2 can point it at a markdown file. |
| `crates/ebook` | 4 | markdown → EPUB → MOBI/AZW3. Knows nothing about PDFs, OCR or models. |

If a change wants to put PDF knowledge in `ebook`, or EPUB knowledge in
`extract`, it belongs in the orchestrator instead.

## The invariant everything else rests on

**Markdown is a real file on disk, not an in-memory handoff.** Layer 2 writes
it; everything after it reads it **back off disk**. Passing the `Document`
straight through would be cheaper and is deliberately not done — the file is the
contract, so a bad OCR page can be fixed in a text editor and the ebook rebuilt
without re-running OCR.

Do not "optimise away" that round trip.

There are **two** such files on a proofread run, and the LLM pass sits between
them:

```
book.pdf ──► book.md ──► book.proofread.md ──► book.epub ──► book.mobi
             extracted   only when the model actually ran
```

The model is given the file, not the in-memory document, and its answer is
another file — so `diff book.md book.proofread.md` is the whole audit of what it
did, and either file can be hand-edited and rebuilt from. The ebook is built
from the proofread file when there is one, and from `book.md` when there is not
(mode `never`, nothing matched the filter, or ollama was unreachable — the last
warns and carries on). `Outcome.markdown_path` is always the extracted file;
`Outcome.proofread_path` is `Some` only when the second one was written.

Its direct consequence: a markdown file *is* the intermediate form, so it can be
fed in as an input, entering the pipeline at the proofreading step.
`core::InputKind` (from the file extension) is what layer 2 dispatches on.

The format is CommonMark plus two conventions: `#` opens a chapter (a new XHTML
file in the EPUB), `##` is a subtitle, and `<!-- page: N -->` marks a printed
page boundary. The page marker must stay legal **inline**, because printed pages
usually break mid-paragraph and the paragraph is kept whole.

## CLI

Binary: `pdftomobi` (`crates/cli`). `--help` is accurate and worth reading.

```sh
# From a PDF — the whole pipeline
pdftomobi book.pdf --lang tur --format md,epub
pdftomobi book.pdf --lang eng --format epub,mobi --llm suspicious
pdftomobi book.pdf --llm always --ollama-url http://desktop:11434,http://laptop:11434
pdftomobi book.pdf --pages 40-45 --format md          # fast iteration while tuning

# From markdown — no extraction, but the model still runs if asked
pdftomobi notes.md --format epub
pdftomobi book.md --format mobi,azw3 --title "A Title" --author "A Name"
pdftomobi book.md --llm always --format epub   # proofread an earlier run's markdown
```

- `--format` is a comma list: `md, epub, mobi, azw3, json`. Markdown is written
  whenever any ebook format is requested, because layer 4 needs it.
- `--out` with an extension names the file; without one it is a directory.
- Anything but `--llm never` writes `<stem>.proofread.md` and builds the ebook
  from it. The extracted `<stem>.md` is kept either way.
- Every run writes `<stem>.report.md` unless `--no-report`: what was removed as
  chrome, every correction the model made or refused, and all warnings.
- A Kindle format always goes through an EPUB, even if none was asked for; the
  intermediate is written next to the output and deleted afterwards.

### Markdown input specifics

- Options that drive extraction (`--ocr`, `--dpi`, `--crop-*`) are inert. The
  CLI prints `!! --dpi applies to a PDF input only; ignored` rather than
  swallowing them. `--pages` and `--format json` describe PDF pages and are
  rejected outright.
- `--llm` is **not** inert: proofreading reads markdown and writes markdown, so
  it runs here on the same terms, into `<stem>.proofread.md`. The default
  (`auto`) still does nothing, because it means "only what we OCR'd ourselves"
  and nothing was OCR'd — `--llm always` or `--llm suspicious` is the way in.
- `--title` / `--author` are explicit overrides and beat the front matter.
  `--lang` has a default and cannot be told apart from one, so it only supplies
  a language the front matter does not already state.
- `--format md` on a markdown input rewrites the file in canonical form. It is
  **skipped with a warning** when the output path is the input path, since that
  would truncate the source. `--out` gives it somewhere to go.
- The report omits its Pages and chrome sections — they would be rows of zeroes
  about work that never happened. The proofreading section follows the model,
  not the input kind, so it appears whenever the model ran.

The GUI (`pdftomobi-gui`) is **PDF only**; its picker filters to `.pdf`. The
orchestrator would handle markdown for it already if the filter were widened.

## External tools

All optional, each needed only for one path. A born-digital PDF to EPUB needs
none of them.

| For | Needs | Override |
|---|---|---|
| Pages with no text layer | `tesseract` + language data | `TESSERACT_BIN` |
| MOBI / AZW3 | `calibre` | `EBOOK_CONVERT` |
| Proofreading | `ollama` serving the model | `--ollama-url`, `--llm-model` |

`--ollama-url` takes a **comma-separated list** of servers. One request is in
flight per entry and the batches are pulled from a shared queue, so a faster box
simply takes more of them; the same URL twice gives that server two workers. A
server that fails preflight, or that stops answering mid-run, is dropped with a
warning and its work goes to the others — it is only an error when none is left.

pdfium is downloaded and cached on first run (`pdfium-bundled`). On macOS
calibre's binary lives inside the app bundle and is not on `PATH`;
`ebook::mobi::find_converter` checks `/Applications/calibre.app/...` explicitly,
so `which ebook-convert` failing does not mean MOBI is unavailable.

Model replies are cached in `~/Library/Caches/pdftomobi/llm/<model>/`, keyed by
`blake3(prompt version + text)`. A re-run of a long book costs nothing;
`--no-cache` bypasses it.

## Tests

Inline `#[cfg(test)] mod tests` at the bottom of the file under test. Every test
in the workspace is a unit test; `crates/extract/tests/` exists but is empty, so
there is no integration-test convention to follow yet. Temp files follow the
house pattern:

```rust
let dir = std::env::temp_dir().join(format!("pdftomobi-<what>-{}", std::process::id()));
```

Test names are full sentences describing the behaviour
(`markdown_output_over_the_input_is_skipped_not_truncated`), not
`test_foo`. Every fix in `docs/findings.md` has a test; keep that true.

The seven PDF fixtures in `test/` each exercise a different failure mode — see
[`test/README.md`](test/README.md) before adding another.

The Turkish book (`Jane Corry - Kocamın Karısı.pdf`), the two markdown extracts
cut from it, and the whole of `test/groundtruth/` were **removed on 2026-09-05**,
deliberately. What was measured on that book stays in `docs/findings.md` — it is
a record of runs that happened, not something reproducible here any more. Two
consequences worth knowing before you trust a number:

- **Proofreading quality is a judgement call again.** The CER scoring in
  §4.5 of `docs/findings.md` needs the two markdown extracts and their hand
  proofreads, and none of the four are in the repo. Re-tuning the prompt means
  rebuilding an equivalent set first — tuning and held-out both, for the reason
  §4.5 gives.
- **So is OCR accuracy.** `groundtruth/lady-susan-p2-9.lines.txt` and
  `groundtruth/under-lock-and-key.archive-ocr.txt` went with the directory even
  though neither came from the Turkish book. The PDFs they score are both still
  here, and `test/README.md` records how each was produced.

`under-lock-and-key.pdf` is a **stress case, not a target to pass**. Its second
column lost most of its spaces in the original scan's OCR. Treat regressions
there as informational.

## Things that will bite

- `out/` and `target/` are gitignored, as are `*.epub`, `*.mobi`, `*.azw3` and
  `*.report.md`. `out/` holds outputs from earlier runs over the fixtures — they
  are useful as markdown inputs for testing layer 4, but they are not fixtures
  and are not checked.
- Calibre's KF8 writer **discards** the EPUB pagebreak anchors and `page-list`.
  EPUB is the format that preserves page breaks; `--page-breaks hard` is the
  workaround for Kindle. Both front ends say so. This is verified, not assumed.
- Everything downstream of extraction works in **PDF points, top-left origin**.
  pdfium reports points bottom-up and tesseract reports pixels top-down; both
  are converted at the boundary. Do not introduce a third convention.
- Page sizes drift *within* a single document (474×788 → 545×866pt in the
  Turkish book), so margins and indent thresholds are per page. Never hardcode
  a constant there.
- The LLM pass is deliberately independent of the OCR decision: a PDF's text
  layer can itself be OCR output (Acrobat ClearScan) and carry OCR typos even
  when we never run OCR on it.

## Claims

State what was verified against a real run or a test and what was not. "It
builds" is not "it works" — this project's docs quote measured numbers
(99.26% word accuracy, 6.7s for 419 pages), so an unlabelled guess stands out
badly next to them.
