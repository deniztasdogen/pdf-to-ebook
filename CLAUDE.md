# pdf-to-ebook — working notes

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
cargo test --workspace          # 143 tests, all should pass
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
| `crates/core` | 0 | `Config`, `Document`/`Block`/`Span`, `Error`, progress events, and `env.rs` (`.env` + machine settings). **No PDF, OCR, HTTP or UI code** — that is what lets the GUI depend on it without pulling in pdfium. |
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

Binary: `pdf-to-ebook` (`crates/cli`). `--help` is accurate and worth reading.

```sh
# From a PDF — the whole pipeline
pdf-to-ebook book.pdf --lang tur --format md,epub
pdf-to-ebook book.pdf --lang eng --format epub,mobi --llm suspicious
pdf-to-ebook book.pdf --llm always --ollama-url http://desktop:11434,http://laptop:11434
pdf-to-ebook book.pdf --pages 40-45 --format md          # fast iteration while tuning

# From markdown — no extraction, but the model still runs if asked
pdf-to-ebook notes.md --format epub
pdf-to-ebook book.md --format mobi,azw3 --title "A Title" --author "A Name"
pdf-to-ebook book.md --llm always --format epub   # proofread an earlier run's markdown
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

The GUI (`pdf-to-ebook-gui`) is **PDF only**; its picker filters to `.pdf`. The
orchestrator would handle markdown for it already if the filter were widened.

## The two extension points

Both are files. Neither needs a rebuild, and both are read through
`core::env`, so a `.env` reaches them.

### `.env` — machine settings

`.env.example` is the documented list; `.env` is gitignored. Precedence is
**flag, then real environment variable, then `.env`, then the compiled-in
default in `core::env::defaults`**.

`PDF_TO_EBOOK_OLLAMA_URL` (falls back to `OLLAMA_HOST`),
`PDF_TO_EBOOK_LLM_MODEL`, `PDF_TO_EBOOK_OCR_LANG`, `PDF_TO_EBOOK_DPI`,
`PDF_TO_EBOOK_PROMPT_DIR`, `PDF_TO_EBOOK_CACHE_DIR`, `TESSERACT_BIN`,
`EBOOK_CONVERT`. `PDF_TO_EBOOK_ENV_FILE` names a `.env` outright; otherwise
the nearest one at or above the working directory is used.

Two rules that are load-bearing, not style:

- **`Config::new` must never read the environment.** It uses the compiled-in
  defaults; `Config::with_defaults` is what layer 1 calls. Wire a new setting
  into `Defaults` and the front ends, not into `Config::new`, or every test
  that builds a `Config` starts depending on the developer's `.env`.
- **Never `std::env::set_var`.** The file is parsed into a map and read from
  there. OCR and proofreading run on thread pools, and mutating the process
  environment underneath them is unsound.

Only settings about the **machine** belong here. `--format`, `--pages` and the
crop describe the book in front of you.

### `prompts/` — the proofreading prompt

```
prompts/proofread.md          general: intro, rule 0, fault classes, limits
prompts/languages/<code>.md   what is only true of that language
prompts/languages/README.md   how to add one
```

`## name` — exactly two hashes — opens a section; every other heading level is
prose and is dropped. Text before the first `-` is the section's lead-in, one
paragraph. Each `-` is one rule, rejoined from however far it wrapped. Rules
are **numbered at render time**, continuously across sections, so a language
pack splices into the middle without renumbering and drops out without a gap.
A section with no rules is skipped lead-in and all.

- Both files are `include_str!`'d, so an installed binary needs no `prompts/`
  next to it. `PDF_TO_EBOOK_PROMPT_DIR` overrides them **per file**.
- A language pack is looked up by tesseract code, the same one `--lang` takes.
  `## label` is optional when the code is in `LANGUAGES`; give it to use a code
  the table has never heard of.
- **The cache key carries `blake3` of the rendered prompt.** `PROMPT_VERSION`
  now only tracks the *request* shape. Do not go back to bumping an integer for
  a prompt change — a file can be edited between two runs and an integer cannot
  notice.
- Tests assert against `Prompts::builtin()`, never `Prompts::load()`, so a
  developer with `PDF_TO_EBOOK_PROMPT_DIR` set still gets an honest
  `cargo test`. The override mechanism has its own tests, against a temp
  directory.
- `cargo test -p pdf-to-ebook-extract show_the_prompt -- --ignored --nocapture`
  prints the rendered prompt.

Re-tuning is not free: see `docs/findings.md` §4.5 and the note below about the
scoring corpus having been removed. Rule 0 must precede the quote rules and the
limits must come last — both were measured, and both lost the other way round.

## External tools

All optional, each needed only for one path. A born-digital PDF to EPUB needs
none of them. Every override below is read through `core::env`, so it can be
set in `.env` as well as in the environment.

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

Model replies are cached in `~/Library/Caches/pdf-to-ebook/llm/<model>/`
(`PDF_TO_EBOOK_CACHE_DIR` moves it), keyed by
`blake3(prompt fingerprint + text)` where the fingerprint covers the request
shape *and* the rendered prompt. A
re-run of a long book costs nothing; `--no-cache` bypasses it, and editing
anything in `prompts/` invalidates it by construction.

## Tests

Inline `#[cfg(test)] mod tests` at the bottom of the file under test. Every test
in the workspace is a unit test; `crates/extract/tests/` exists but is empty, so
there is no integration-test convention to follow yet. Temp files follow the
house pattern:

```rust
let dir = std::env::temp_dir().join(format!("pdf-to-ebook-<what>-{}", std::process::id()));
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

- `out/` and `target/` are gitignored, as are `.env`, `*.epub`, `*.mobi`,
  `*.azw3` and `*.report.md`. `out/` holds outputs from earlier runs over the fixtures — they
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
