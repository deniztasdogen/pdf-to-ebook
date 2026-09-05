# Architecture

Four layers, as requested. Each is a separate crate and depends only on the
layer below it plus the shared types.

```
crates/gui   crates/cli          Layer 1 — input
      \        /                 pick an input, a language, output formats
       \      /
   crates/orchestrator           Layer 2 — orchestration
            |                    owns the order of operations, nothing else
            |
    ┌───────┴────────┐
    │                │
crates/extract   crates/ebook    Layer 3 — extraction    Layer 4 — ebook
 PDF -> markdown   markdown ->    text layer, OCR,        markdown -> EPUB
                   EPUB/MOBI      layout, LLM pass        -> MOBI/AZW3
            |
      crates/core                 shared types only
                                  (no PDF, OCR, HTTP or UI code)
```

`crates/core` exists so layer 1 can describe a job without pulling in pdfium or
tesseract. It holds `Config`, the `Document` model, the error type and the
progress events.

## The markdown boundary is a real file

Layer 2 writes the markdown to disk and everything after it reads it **back off
disk**. It would be cheaper to pass the `Document` in memory, and that is
deliberately not done: the markdown is the contract, so it can be inspected,
diffed, and corrected by hand between extraction and ebook building. A bad OCR
page can be fixed in a text editor and the EPUB rebuilt without re-running OCR.

The LLM pass sits between two such files rather than inside the extractor:

```
book.pdf ──► book.md ──► book.proofread.md ──► book.epub ──► book.mobi
             extracted   written only when the model actually ran
```

The model is handed the file, not the document that produced it, and its answer
becomes a file of its own. That buys three things a purely in-memory pass did
not: `diff book.md book.proofread.md` is a complete audit of what the model did,
a bad correction can be fixed by hand and the ebook rebuilt without going near
ollama again, and the same pass works on a markdown file that was never a PDF.

Layer 4 reads the proofread file when there is one and `book.md` when there is
not. There is no second file when the mode is `never`, when nothing matched the
filter, or when ollama could not be reached — the last of those warns and the
run continues, because a missing model must not throw away a good extraction.

The format is plain CommonMark plus two conventions:

```markdown
---
title: Kocamın Karısı
author: Jane Corry
language: tr
source: Jane Corry - Kocamın Karısı.pdf
pages: 419
generator: pdftomobi 0.1.0
---

# 44

## Carla

Tabii ki yeni resmin tanıtımı onları bir<!-- page: 302 --> araya getirmekte etkili olmuştu.
```

`#` opens a chapter (a new XHTML file in the EPUB), `##` is a subtitle, and
`<!-- page: N -->` marks a printed page boundary.

The page marker is an HTML comment so it survives round-tripping and stays
invisible in any markdown viewer. It has to be legal *inline*, because printed
pages usually break mid-paragraph — see below.

### …which means markdown is also an input

Because the boundary is a real file, a markdown file can be fed to the tool
directly. It does not go through a converter first: it *is* the intermediate
form, so it enters at the proofreading step and nothing extracts or OCRs it.

```
book.pdf ──► layer 3 ──► book.md ──► LLM ──► book.proofread.md ──► layer 4 ──► .epub
                             ▲
book.md  ────────────────────┘   nothing extracts or OCRs it
```

Layer 2 decides which of the two entry points to use from the input's
extension (`core::InputKind`), so both front ends get this for free:

```sh
pdftomobi notes.md --format epub
pdftomobi book.md --format mobi --title "Kocamın Karısı" --author "Jane Corry"
```

Consequences worth knowing:

- Every option that drives extraction — `--ocr`, `--dpi`, `--crop-*` — is
  inert. The CLI says so rather than accepting them silently. `--pages` and
  `--format json` describe PDF pages, so they are rejected outright.
- `--llm` is the exception, because the pass takes markdown and returns
  markdown. `--llm always` on a `.md` proofreads it into `<name>.proofread.md`
  and builds the book from that. The default, `auto`, still does nothing: it
  means "the pages we OCR'd ourselves", and there were none.
- `--title` and `--author` are explicit overrides and beat the front matter.
  `--lang` has a default and cannot be told apart from one, so it only supplies
  a language the front matter does not already state.
- `--format md` on a markdown input rewrites the file in the canonical form.
  Writing it back over the input would truncate the source, so that case is
  skipped with a warning; `--out` gives it somewhere to go.
- The report drops its Pages and chrome sections, which would otherwise be rows
  of zeroes about work that never happened. Its proofreading section follows the
  model rather than the input kind, so it appears whenever the model ran.

Verified: `lady-susan.md` → EPUB and `lady-susan.pdf` → EPUB produce
byte-identical chapter XHTML, 42 chapters and 50 page anchors either way.

## Layer 3: extraction

Both input paths converge on one intermediate representation as early as
possible, so the hard layout logic is written once.

```
                  ┌──────────────────────────┐
   PDF ──┬────────►│ text layer (pdfium)      │──┐
         │         │ chars + loose_bounds     │  │
         │         └──────────────────────────┘  │
   per-page                                      ├──► PageLayout ──► analysis
   decision      ┌──────────────────────────┐    │   (points, top-left)
         └───────►│ OCR                      │───┘
                  │ pdfium render → tesseract│
                  └──────────────────────────┘
```

Everything downstream works in **PDF points with a top-left origin**. pdfium
reports points bottom-up, tesseract reports pixels top-down; both are converted
at the boundary. Points are kept rather than normalised to 0..1 because every
threshold is derived from per-page statistics anyway, and dividing x by width
while dividing y by height would quietly make horizontal and vertical
measurements incomparable.

### Stages

| # | Stage | File |
|---|---|---|
| 0 | Decide per page: text layer, OCR, or try both | `pdf.rs` |
| 1a | Text-layer words | `pdf.rs` |
| 1b | Render at 300 dpi, run tesseract, parse TSV | `ocr/` |
| 2 | Column gutters, settled document-wide | `layout/columns.rs` |
| 3 | Running headers, footers, page numbers, UI chrome | `layout/chrome.rs` |
| 4 | Lines within each column | `layout/lines.rs` |
| 5 | Paragraphs and dehyphenation | `layout/paragraphs.rs` |
| 6 | Headings and chapters | `layout/headings.rs` |
| 7 | Cross-page joins and page markers | `layout/mod.rs` |
| 8 | Markdown | `markdown.rs` |

Proofreading is **not** in that list: layer 2 runs it, over the markdown stage 8
wrote. `proofread/` still lives in layer 3, because it is knowledge about
repairing text rather than about ebooks; what moved is who calls it and what it
is pointed at.

Stage order matters in two places that are easy to get wrong.

**Columns before lines.** Grouping words into lines across a two-column page
interleaves the columns, because the two columns' baselines do not align. On
page 5 of `under-lock-and-key.pdf` that produced lines alternating between left
and right.

**Gutters before chrome.** The gutters are needed twice — to recognise
furniture that crosses one, and to split the body afterwards — so they are
derived once and shared.

### Page classification: try both when unsure

Three outcomes per page: trust the text layer, OCR it, or **do both and keep
whichever reads better**.

The third case exists because guessing wrong is expensive in both directions.
The title page of `lady-susan.pdf` has a perfectly good 27-character text layer,
and OCR reads its "Jane" as "Fane" — a bare character threshold would silently
corrupt the author's name. But a scanned page can also carry a stray fragment of
real text over an un-OCR'd image, where the text layer is useless. Only a
handful of pages per book are ambiguous, so trying both is affordable.

OCR has to beat the text layer by 1.5x on character count to win.

### Paragraphs: three independent signals

No single signal covers the corpus.

1. **First-line indent** — strong and clean, but the threshold must be computed
   per page. Page 40 of the Turkish book has margin 31 / indent 54; page 300 has
   28.5 / 47.
2. **The previous line stopped short of the measure** — the *only* signal that
   works on `lady-susan.pdf`, whose paragraphs are block-style with no indent.
3. **An unusually large vertical gap.**

Plus two exceptions that stop the indent rule misfiring: a **centred final
line** is inset from the left exactly like an indent, and a **right-aligned
dateline** is its own block rather than the opening of the paragraph below it.

### Page breaks: joined, not split

A paragraph that runs across a page boundary is **joined**, and the marker is
inserted inline at the exact join. Splitting at every boundary would introduce
419 false paragraph breaks in the Turkish book.

Joining is refused when the previous page held fewer than five lines, so a title
page or part divider is not welded to the next page's opening paragraph.

`--page-breaks` selects the policy:

| Value | Behaviour |
|---|---|
| `anchors` (default) | inline EPUB 3 anchors plus a `page-list` nav; reflow preserved |
| `hard` | CSS `page-break-before`; survives MOBI but fights reflow |
| `none` | discard page information |

### The LLM pass

Runs from layer 2, over the markdown file, and writes `<name>.proofread.md`. Its
input is a `Document` parsed back off disk, so the only extraction provenance it
still has is one bit — did this run OCR anything — which is all the filter modes
ever used: span-level provenance was never tracked.

Independent of the OCR decision, because a PDF can carry a text layer that is
*itself* OCR output — the Turkish book was processed with Acrobat ClearScan, so
it has OCR typos even though we never run OCR on it.

Paragraph text only. Headings are left alone: the EPUB's chapter list is built
from them, and a "correction" there is far likelier to be a rewrite than a fix.

| `--llm` | Behaviour |
|---|---|
| `auto` (default) | paragraphs from pages we OCR'd ourselves |
| `suspicious` | paragraphs that look damaged (odd characters, welded or shattered words, low OCR confidence) |
| `always` | everything |
| `never` | off |

Four safeguards, each answering a measured failure — see `findings.md` §4:

1. `"think": false` is mandatory (without it, replies come back empty).
2. Batch length is validated; a mismatch re-runs the batch one at a time.
3. An edit-distance guard refuses anything that drifts too far from the original.
4. Results are cached on disk, keyed by `blake3(model + prompt version + text)`.

#### The prompt is in two halves

The system prompt is assembled, not written out whole:

```text
header + rule 0  ─┐
general rules     ├─►  system_prompt()      proofread/mod.rs
language rules    │    ▲
limits           ─┘    │  --lang / GUI language box ─► Config::lang ─► prompt_language()
```

`proofread/mod.rs` owns the part that is true of any Latin-script scan: the
page-fragment rule, the mangled quotation marks, the letter-shape confusions,
the limits. `proofread/language.rs` owns the part that is only true of one
language, keyed by the tesseract code the user already picks — for Turkish,
lost `ğ`, dotted `İ`, the apostrophe before a case suffix, and the measured
Turkish examples. The rules are numbered at assembly time, so a language pack
can be spliced in without renumbering anything and left out without a gap.

A language with no pack (every one but Turkish today) gets the general rules and
its own name, which is what every language got before the split. The ordering
that `findings.md` §4.5 measured is preserved: rule 0 first, the limits last.

#### Several servers at once

`--ollama-url` takes a comma-separated list, and the pass runs one request in
flight per entry:

```sh
pdftomobi book.pdf --llm always \
  --ollama-url http://desktop:11434,http://laptop:11434
```

```text
paragraphs ─► batches of 10 ──► ┌── queue ──┐ ── worker ──► desktop
                                └───────────┘ ── worker ──► laptop
```

The batches are **pulled, not dealt**. A worker takes the next one when it is
free, so a machine twice as fast takes twice as many and the two finish
together; dealing halves out in advance would leave the desktop idle waiting
for the laptop. The report prints the resulting split per server, which is
uneven by design. Listing the same URL twice gives that server two workers,
which is how to use a box running with `OLLAMA_NUM_PARALLEL=2`.

Servers are allowed to be absent or to disappear, on the same principle that
already governs a missing ollama — a model that is not there must not throw away
a good extraction:

- Preflight asks every server at once. One that does not answer, or has not
  pulled the model, is dropped with a warning and the run goes ahead on the
  rest. It is an error only when none is left, which for a single server is the
  old behaviour exactly.
- A batch that fails mid-run is still retried one paragraph at a time on the
  same server, because one slow batch is usually a busy server, not a dead one.
  Only when not one of those singles gets through is the server presumed gone:
  its batch goes back on the queue for another worker and it retires.
- If every server goes away, the paragraphs no one reached keep their original
  text and the report counts them. Nothing is lost but the repairs.

Verified in tests against fake servers: two servers are proved to be in flight
at the same moment, and a server killed after preflight has its batch finished
by the other. The wall-clock gain on real hardware has **not** been measured —
there is one ollama box here.

## Layer 4: ebook building

Reads the last markdown on disk — `<name>.proofread.md` if the model ran, the
extracted one otherwise — writes an EPUB 3, then hands that to calibre's
`ebook-convert` for MOBI/AZW3.

A Kindle format still goes through an EPUB even when no EPUB was asked for; the
intermediate is written next to the output and deleted afterwards.

The EPUB is written by hand with the `zip` crate rather than with
`epub-builder`, because the one feature this project exists to get right — the
`page-list` nav recording where the printed pages fell — is not something that
crate models. The container layout is verified against calibre: `mimetype`
first and stored uncompressed, then `META-INF/`, then `OEBPS/`.

**Verified caveat:** calibre's KF8 writer discards the pagebreak anchors and the
`page-list`. EPUB is the format that preserves page breaks; the CLI and the GUI
both say so when you ask for a Kindle format.

## Layer 1: two front ends, no logic

Both build a `Config` and hand it to layer 2. Neither knows whether the input
is a PDF or a markdown file — layer 2 dispatches on that.

- `crates/cli` — `pdftomobi`, takes a `.pdf` or a `.md`, with `--pages` for fast
  iteration while tuning.
- `crates/gui` — `pdftomobi-gui`, PDF only (its picker filters to `.pdf`), an
  egui window: file picker (and drag-and-drop),
  language, output formats, a "More options" panel, a progress bar and a log.

The GUI runs the conversion on a worker thread and communicates over a channel.
Its `Reporter` implementation forwards progress events to the UI and carries the
cancel flag, so a long run can be stopped.
