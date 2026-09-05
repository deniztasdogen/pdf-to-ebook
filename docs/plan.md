# pdf-to-ebook — the original plan

Date: 2026-09-04. **Kept for the record.** This is the plan as written before
any code existed. For what was actually built, read
[`architecture.md`](architecture.md); for the evidence,
[`findings.md`](findings.md).

## What changed once it was implemented

The plan held up in the large — one intermediate representation, per-page
statistics, joined page breaks, four LLM safeguards — and was wrong in five
places worth naming:

1. **Layering.** The plan had a single `extract` crate and passed data in
   memory. The delivered design is four layers with **markdown as a real file**
   between extraction and ebook building, so a bad page can be hand-corrected
   and the EPUB rebuilt without re-running OCR. The LLM pass was later moved out
   from between them to *after* the markdown is written: it reads that file and
   writes `<name>.proofread.md`, and the ebook is built from the second file.
   Two files, one diff, and the pass now works on a markdown input too.
2. **Stage order.** The plan assembled lines and *then* split columns. That is
   backwards: clustering words by y across a two-column page interleaves the
   columns. Columns first, and gutters before chrome, because gutters identify
   furniture that crosses one.
3. **Page classification.** The plan used one character threshold. That sends a
   sparse-but-valid title page to OCR, which read "Jane" as "Fane". Ambiguous
   pages now run both and keep whichever reads better.
4. **Column detection.** The plan sized gutters as a fraction of page width. A
   real CVPR gutter is under 4%, so that rejected the genuine article; gutters
   are sized in ems. Column count is also settled document-wide, not per page.
5. **Word grouping was not in the plan at all**, and turned out to be where most
   of the difficulty lived — see `findings.md` §7.1-7.5. ClearScan's glyph
   clusters, phantom glyphs and font-size-1.0 spaces each silently corrupted the
   whole book.

Also added along the way: bold-based heading detection for non-fiction, fuzzy
matching for running headers, and two structural rules for screenshot chrome.

---

## 1. Answer to the language question

**Rust is enough. No hand-written C++ is needed.**

The two heavy libraries are C/C++, but both are consumed as prebuilt binaries
through existing bindings — we never write or compile C++ ourselves:

- **pdfium** (C++, BSD-3) via `pdfium-render` — text geometry *and* page
  rasterisation from one dependency. Verified building and running on this
  machine.
- **tesseract/leptonica** (C++, Apache-2.0) — invoked as an already-installed
  CLI in v1, parsing its TSV output.

The single genuine gap is MOBI packaging: no usable native Rust MOBI writer
exists. We shell out to calibre's `ebook-convert`, which is already installed.
Everything upstream of that — parsing, layout analysis, paragraph
reconstruction, the LLM pass, EPUB writing — is pure Rust.

---

## 2. The central design idea: one intermediate representation

The text path and the OCR path must converge as early as possible, so that all
the hard logic (chrome removal, columns, paragraphs, hyphenation, headings) is
written **once** and behaves identically regardless of source.

```
                 ┌─────────────────────────┐
  PDF ──┬───────►│ text path (pdfium)      │──┐
        │        │ chars + loose_bounds    │  │
        │        └─────────────────────────┘  │
        │                                     ├──► PageLayout ──► shared analysis
        │        ┌─────────────────────────┐  │    (normalised)
        └───────►│ OCR path                │──┘
   per-page      │ pdfium render 300dpi    │
   decision      │ → tesseract TSV         │
                 └─────────────────────────┘
```

### The types

```rust
/// Normalised, top-left origin, units = fraction of the page box.
/// Normalisation is essential: page sizes vary from 474x788 to 545x866 pt
/// within a single document (findings §3.4).
struct Rect { x0: f32, y0: f32, x1: f32, y1: f32 }

struct Word {
    text: String,
    bbox: Rect,
    font_size: f32,          // normalised to page height
    confidence: Option<f32>, // Some(_) only on the OCR path
}

struct Line {
    words: Vec<Word>,
    bbox: Rect,
    baseline_y: f32,
    median_font_size: f32,
    ends_hyphenated: bool,   // U+0002 / U+00AD / trailing '-'
}

struct Column { lines: Vec<Line>, bbox: Rect }

struct PageLayout {
    pdf_index: usize,
    source: PageSource,           // Text | Ocr
    size_pt: (f32, f32),
    columns: Vec<Column>,         // in reading order
    printed_label: Option<String>,// recovered from the footer, e.g. "41"
    dropped: Vec<DroppedLine>,    // header/footer/chrome, kept for the report
}
```

Two rules that make the rest of the pipeline sane:

1. **Everything is normalised to the page box.** pdfium is bottom-left in
   points; tesseract is top-left in pixels. Both are converted to a top-left
   system in fractions of page width/height at the boundary. Nothing downstream
   sees raw coordinates.
2. **Per-page statistics, never document constants.** Margins, indent
   thresholds and band heights are all derived from the page being processed.

---

## 3. Pipeline stages

### Stage 0 — Load and classify each page

Per page, ask pdfium for the extractable character count and the page object
types.

```
source = if extractable_chars >= TEXT_THRESHOLD { Text } else { Ocr }
```

Notes from the evidence:

- Decide **per page**, not per document. In the Jane Corry book 414 pages have a
  good text layer and 5 (indices 0, 2, 4, 7, 418) do not — all front/back matter.
- A page having images means nothing. 310 of 419 pages carry images *and*
  1,400–2,000 characters of good text. Prefer text whenever text exists.
- Images are never extracted. The user only wants text.
- `--ocr never|auto|always` overrides the decision.

The threshold should be density-based (characters relative to page area) rather
than a bare count, so a sparse-but-real page is not misrouted. Calibrate against
the observed spread: body pages 1,400–2,000 chars, title pages 30–118, blanks 0.

### Stage 1a — Text path

- Iterate `page.text().chars()`.
- Use **`loose_bounds()`**. Never `tight_bounds()` — it is the ink box and
  shatters every line containing a descender or a Turkish diacritic
  (findings §3.1). This one detail decides whether the text path works at all.
- Filter out pdfium's synthetic `\r`/`\n` characters, identifiable by
  `scaled_font_size() == 1.0` and a degenerate box. They poison margin and line
  statistics if left in (findings §3.2).
- Record `U+0002` at end of line as the hyphenation marker; also accept
  `U+00AD` and a literal trailing `-` (findings §3.3).
- Font name is available from pdfium. Capture it now even though v1 does not use
  it — it is the hook for italic recovery later.

### Stage 1b — OCR path

- Render the page with pdfium at `--dpi` (default 300). Measured at 70 ms for a
  full page, so this is not a bottleneck.
- Optional, off by default: grayscale + deskew. The e-reader screenshot needed
  no preprocessing. Add it only when a real page demands it.
- Run `tesseract <img> stdout -l <lang> --psm 1 tsv`. `--psm 1` (auto page
  segmentation with OSD) is what correctly ordered the two columns of the Dutch
  screenshot.
- Parse the TSV into `Word`s. Keep `conf` per word — it is what drives the
  "only proofread doubtful paragraphs" filter later.
- Put the OCR engine behind a trait:

  ```rust
  trait OcrEngine { fn recognise(&self, img: &RgbImage, lang: &str) -> Result<Vec<Word>>; }
  ```

  v1 ships `TesseractCli` (zero build risk, already installed). `leptess` FFI
  can slot in later; I did not verify that build.

**`ocrs` is not an option.** Its recognition model's alphabet is hardcoded ASCII
with no `ı ğ ş ç ö ü` and no `ë ï é`, so it cannot read either of our test
languages (findings §2).

### Stage 2 — Line assembly

Sort glyphs/words by y then x, then cluster into lines.

The y-tolerance must be **proportional to the median glyph height**, not a
constant. With a fixed 4.0 pt tolerance, scan skew on page 300 split `dan` off
its own line, 5.3 pt below the rest (findings §3.6). After clustering, sort
fragments by x within the line — the stray fragment belonged at the *start* of
its line, not the end.

### Stage 3 — Chrome, running headers and footers

Two mechanisms, because the two test files need different ones.

**Cross-page repetition (multi-page documents).** For candidate lines in the top
and bottom ~8% bands, normalise the text (lowercase, strip digits, collapse
whitespace) and count occurrences across all pages. A normalised string
appearing in the same band on ≳25% of pages is a running header or footer.
This catches `JANE CORRY ll KOCAMIN KARISI`, which is on essentially every page.

**Printed page label.** A short, mostly-numeric line in the bottom band is the
printed page number. Capture it as `printed_label` and remove it from the body.
Verified: `·41 -` on PDF index 40 — so the printed label is offset from the PDF
index, and the PDF's own page labels (`1,2,3,...`) are no help (findings §3.7).
Fall back to `pdf_index + 1` when no label is found.

**Single-page documents** cannot use repetition. `bookSample.pdf` needs its
UI bar, progress bar and nav arrows removed with one page of evidence. Use
geometric heuristics — isolated short lines in the outer bands, lines separated
from the body by a horizontal rule, content outside the dominant text block —
and expose `--crop-top` / `--crop-bottom` / `--crop-left` / `--crop-right` as
explicit escape hatches.

Every dropped line is recorded in `PageLayout::dropped` and listed in the run
report, so nothing disappears silently.

### Stage 4 — Column detection

Build a vertical projection of word coverage across x, restricted to the body
band (after Stage 3, so headings and headers do not create false gaps). A
contiguous x-range with no coverage that is wider than ~5% of page width and
spans more than ~60% of the body height is a column separator.

Order columns left to right and concatenate their lines. Needed for
`bookSample.pdf` (2 columns); a no-op for the Turkish book (1 column).

Cross-check against tesseract's own `block_num`, which already segmented the two
columns correctly. Where our detection and tesseract's disagree, log it.

### Stage 5 — Paragraph grouping

This is the stage that determines output quality.

Per column, compute the **modal left edge** of its lines — that is the body
margin. Then start a new paragraph when any of these holds:

1. **First-line indent.** `line.left_x > margin + 0.5 × median_font_size`.
   The strongest signal (findings §3.5). The threshold must be computed per
   page: page 40 has margin 31 / indent 54, page 300 has margin 28.5 / indent 47
   (findings §3.4).
2. **Previous line ended short.** The previous line's right edge is more than
   ~2 × median_font_size short of the column's right edge. This catches
   paragraphs that have no first-line indent.
3. **Vertical gap.** More than ~1.5 × the median line spacing.

Join lines within a paragraph with a single space — except when the previous
line is `ends_hyphenated`, in which case join with no space and drop the marker.

**Dehyphenation nuance.** `U+0002` and `U+00AD` are unambiguous, so join
unconditionally. A literal `-` is ambiguous: `tik-taklarını` is a genuine
compound hyphen and must survive. For a literal `-`, only join when the next
line starts lowercase, and flag the decision in the report.

### Stage 6 — Cross-page paragraph joining and page markers

This is where the user's "page breaks as intended" requirement gets resolved,
and it is more subtle than it first looks.

Paragraphs run across page boundaries. Page 40 ends mid-sentence
(`...Sessizliği bozmaya etor` / `yiy`), continuing onto page 41. If each page
became its own block of paragraphs, every page boundary would introduce a false
paragraph break — roughly 419 of them.

So, after per-page grouping:

- If the last paragraph of page *N* does not end in sentence-final punctuation,
  **and** the first paragraph of page *N+1* is a continuation (not indented),
  merge the two into one paragraph.
- Insert the page marker **inline, at the exact join point**, so the page
  boundary is recorded without breaking the paragraph:

  ```html
  <span epub:type="pagebreak" id="page_41" role="doc-pagebreak" aria-label="41"/>
  ```

This gives reflowable text that still knows where the printed pages fell —
which is what an ebook should do. Every page index gets exactly one marker,
using `printed_label` where recovered.

`--page-breaks` selects the policy:

| Value | Behaviour |
|---|---|
| `anchors` (default) | inline EPUB 3 pagebreak anchors + `page-list` nav; reflow preserved |
| `hard` | CSS `page-break-before: always`; forces real breaks, fights reflow |
| `none` | drop page information entirely |

### Stage 7 — Heading and chapter detection

Signals, in order of trustworthiness:

1. **Horizontal centring** within the column.
2. **Vertical isolation** — large gaps above and below.
3. **Shortness** — well under a full measure.
4. **Pattern match** — `^\d+$`, `^BÖLÜM \d+`, `^HOOFDSTUK \d+`, a bare
   capitalised name.
5. Font size, *weighted last*.

Font size is deliberately demoted. On page 300 the chapter heading reports
size 10.80 while the body reports 13.15 — the heading is **smaller** than the
body text, because ClearScan rescales synthesised glyphs per line
(findings §3.8). Any size-first heuristic gets this page wrong.

The verified structures to reproduce:

```
Jane Corry p300:  "44"           (centred, isolated)  -> chapter number
                  "Carla"        (centred)            -> POV / subtitle
bookSample p0:    "HOOFDSTUK 1"  (centred)            -> chapter number
                  "EEN VRESELIJKE VERJAARDAG"         -> chapter title
```

A detected chapter start opens a new EPUB XHTML file and a new TOC entry.

**Do not use the PDF outline.** It contains 419 bookmarks named
`kocamın karısı - 0001`, one per scanned image — no chapter information at all
(findings §3.9).

### Stage 8 — LLM proofread pass (Ollama, `gemma4:e4b`)

The user's rule is "if OCR is used, verify every paragraph". That is the right
default, but the evidence adds a wrinkle: the Jane Corry text layer is *itself*
OCR output and carries the same class of typos, without any OCR running in our
pipeline. So this is a separate switch:

| `--llm` | Behaviour |
|---|---|
| `auto` (default) | proofread paragraphs from pages where `source == Ocr` |
| `always` | proofread every paragraph, including the text path |
| `suspicious` | proofread paragraphs with low OCR confidence or dictionary misses |
| `never` | skip |

Request shape, all of it load-bearing:

```json
{
  "model": "gemma4:e4b",
  "stream": false,
  "think": false,
  "format": "json",
  "options": { "temperature": 0, "num_predict": 2048 },
  "messages": [ { "role": "system", "content": "<strict proofreader prompt>" },
                { "role": "user",   "content": "{\"paragraphs\": [ ... ] }" } ]
}
```

Four safeguards, each responding to a verified failure:

1. **`"think": false` is mandatory.** `gemma4:e4b` is a thinking model. Left on,
   the answer lands in the `thinking` field and `content` comes back empty —
   2 of 5 test paragraphs returned nothing, at 6–17 s each. Turning it off gives
   ~1 s and zero empties: an 8x speedup that also removes the failure mode
   (findings §4.1).

2. **Validate the batch length.** Batching 10 paragraphs per request is 2.4x
   faster (0.46 s/paragraph), but a test batch returned **9 items for 10 inputs**,
   silently dropping the shortest paragraph (findings §4.2). If
   `out.len() != in.len()`, re-run that batch one paragraph per request. Never
   trust positional alignment without the length check.

3. **Drift guard.** Accept a correction only if it stays close to the original:
   normalised Levenshtein similarity (via `strsim`) ≥ ~0.90 and length change
   ≤ ~15%. On rejection keep the original and log it. This exists because of a
   real observed case: `geç· saatiere` → `geç saate`, which dropped a suffix and
   changed the meaning — a content edit, not a typo fix (findings §4.4). Without
   this guard the pass can quietly damage the book.

4. **Disk cache**, keyed by `blake3(model + prompt_version + paragraph)`. Makes
   re-runs free and the job resumable — important given the runtime.

**Throughput.** Do not parallelise beyond ~4 in flight; the model is GPU-bound
and throughput plateaus at 1.31 req/s (findings §4.3). Budget from the batched
figure: ~0.46 s/paragraph, so roughly **45–90 minutes** for the full 419-page
book on a cold cache, minutes on a warm one.

Emit a **diff report** of every accepted and rejected correction, so the changes
are auditable rather than taken on trust.

### Stage 9 — Output

Canonical intermediate artefacts, written first and always:

- **JSON** — the full `PageLayout` set plus paragraphs. Auditable and diffable.
- **Plain text** — paragraphs separated by blank lines, page markers as comments.

Then:

- **EPUB 3**, hand-written with the `zip` crate. I verified the exact layout
  calibre accepts: `mimetype` stored uncompressed as the first entry, then
  `META-INF/container.xml`, then `OEBPS/` with `content.opf`, one XHTML per
  chapter, and a `nav.xhtml` carrying both `epub:type="toc"` and a hidden
  `epub:type="page-list"`. `epub-builder 0.8.3` was considered and rejected: it
  does not model the `page-list` nav, which is exactly the feature this project
  needs.
- **MOBI / AZW3** via `ebook-convert`. Verified working, and Turkish
  `çğıöşü` survives EPUB → AZW3 → text intact.

**Verified caveat, worth stating plainly:** calibre's KF8 writer **drops** the
pagebreak anchors and the `page-list` nav — grepping the produced AZW3 for
`page_4` returns 0 matches (findings §5). So:

- EPUB 3 is the format that preserves page breaks. It is the primary target.
- For Kindle with genuine breaks, use `--page-breaks hard`, accepting the cost
  to reflow.
- Kindle Previewer 3 honours EPUB 3 `page-list` properly and would be the better
  Kindle route, but it is not installed here so I could not verify it. Tracked
  as a follow-up, not a v1 dependency.

---

## 4. Dependencies

```toml
[dependencies]
pdfium-render  = "0.9"    # text geometry + rasterisation   (MIT/Apache-2.0)
pdfium-bundled = "0.1"    # auto-downloads libpdfium
image          = { version = "0.25", features = ["png"] }
zip            = "2"      # EPUB container
quick-xml      = "0.37"   # XHTML/OPF generation
reqwest        = { version = "0.12", features = ["json"] }
tokio          = { version = "1", features = ["rt-multi-thread", "macros"] }
serde          = { version = "1", features = ["derive"] }
serde_json     = "1"
strsim         = "0.11"   # LLM drift guard
blake3         = "1"      # LLM cache keys
unicode-normalization = "0.1"   # NFC output
rayon          = "1"      # per-page parallelism
clap           = { version = "4", features = ["derive"] }
anyhow         = "1"
thiserror      = "2"
tracing        = "0.1"
tracing-subscriber = "0.3"
indicatif      = "0.17"   # progress for the long LLM pass
```

External tools, all verified present: `tesseract` 5.5.3 (+ `tur`/`nld`),
`ebook-convert` from calibre 8.16.2, `ollama` with `gemma4:e4b`.

### Crate layout

```
src/
  main.rs          CLI
  config.rs        options, resolved policies
  classify.rs      Stage 0 — per-page text vs OCR
  text_path.rs     Stage 1a — pdfium chars -> Word (loose_bounds!)
  ocr/
    mod.rs         OcrEngine trait
    tesseract.rs   TSV parsing, per-word confidence
    render.rs      pdfium page -> RgbImage
  layout/
    geom.rs        Rect, normalisation, page-relative units
    lines.rs       Stage 2 — adaptive y-clustering, x-sort
    chrome.rs      Stage 3 — headers, footers, page labels, UI chrome
    columns.rs     Stage 4 — projection-based column split
    paragraphs.rs  Stage 5 — indent/short-line/gap rules, dehyphenation
    pages.rs       Stage 6 — cross-page joins + inline page markers
    headings.rs    Stage 7 — centring + isolation first, size last
  proofread/
    mod.rs         Stage 8 — batching, length validation, drift guard
    ollama.rs      client (think:false)
    cache.rs       blake3-keyed disk cache
  output/
    doc.rs         chapters, paragraphs, markers
    epub.rs        Stage 9 — EPUB 3 writer with page-list
    mobi.rs        ebook-convert wrapper
    text.rs        plain text + JSON
    report.rs      dropped lines, LLM diffs, warnings
```

---

## 5. CLI

```
pdf-to-ebook <input.pdf> [OPTIONS]

  -o, --out <PATH>            output basename (default: input stem)
  -f, --format <LIST>         epub,mobi,azw3,txt,json   [default: epub,txt]
      --lang <CODE>           OCR language, tesseract code (tur, nld, eng)
      --ocr <MODE>            auto|never|always         [default: auto]
      --llm <MODE>            auto|always|suspicious|never  [default: auto]
      --llm-model <NAME>      [default: gemma4:e4b]
      --ollama-url <URL>      [default: http://localhost:11434]
      --page-breaks <MODE>    anchors|hard|none         [default: anchors]
      --dpi <N>               OCR render resolution     [default: 300]
      --pages <RANGE>         e.g. 40-45 — fast iteration on one spread
      --crop-top <FRAC>       chrome escape hatch (also -bottom/-left/-right)
      --report <PATH>         write the run report
      --no-cache              bypass the LLM cache
```

`--pages` matters more than it looks: it turns a 90-minute full-book run into a
two-second edit-test loop while tuning the layout heuristics.

---

## 6. Milestones

Each milestone ends with a runnable binary and a named test file that must pass.

| # | Deliverable | Proves | Test |
|---|---|---|---|
| **M1** | Skeleton, CLI, text path, line assembly, plain-text output | `loose_bounds` line grouping is correct | `dummy_1.pdf` → `Dummy PDF file` |
| **M2** | Chrome removal, columns, paragraphs, dehyphenation, cross-page joins | the hard layout logic | Jane Corry pp. 40, 300 match golden files |
| **M3** | OCR path into the same IR | both paths converge | `bookSample.pdf` → 2 columns in order, chrome gone |
| **M4** | Heading detection, EPUB 3 writer with `page-list` | structure and page fidelity | full book → valid EPUB, chapter TOC, 419 markers |
| **M5** | Ollama pass: `think:false`, batching, length validation, drift guard, cache | safe, resumable proofreading | known typo list improves; no drift accepted |
| **M6** | MOBI/AZW3 via calibre, run report, full-book run | end to end | 419-page MOBI + report |

M2 is the risky one and carries most of the value. M1 and M3 are largely
plumbing. M5 is bounded by the four safeguards above.

---

## 7. Test strategy

**Golden files.** Commit expected output for a small set of pages that between
them cover every trap: Jane Corry 40 (running header, footer page number,
hyphenation, indent paragraphs), Jane Corry 300 (chapter heading smaller than
body, skew fragment, per-page margin shift), bookSample 0 (two columns, UI
chrome, drop cap), dummy_1 (born-digital baseline).

**Invariants** — cheap checks that catch regressions across the whole book
without hand-maintaining 419 golden files:

- no running-header string appears anywhere in body output
- no paragraph contains a leftover `U+0002` or `U+00AD`
- no paragraph ends with a hyphenation marker
- every PDF page index has exactly one page marker
- paragraph count per page falls in a sane range (flag pages near 1 or near the
  line count — both indicate the paragraph rules misfired)
- output is valid UTF-8 and NFC-normalised
- extracted character count is within a few per cent of raw extraction after
  chrome removal (a large drop means we deleted body text)

**LLM tests** use recorded fixtures, not a live model, so the suite stays fast
and deterministic. Keep one opt-in integration test that hits a real Ollama.

**EPUB validation.** `epubcheck` is not installed; adding it would make M4's
"valid EPUB" claim machine-checked rather than assumed.

---

## 8. Risks and open questions

**Known risks, with mitigations:**

| Risk | Mitigation |
|---|---|
| Paragraph rules misfire on unusual pages | invariant checks flag suspicious pages; `--pages` makes iteration cheap |
| Column projection sees a centred heading as a gap | run column detection only on the body band, after chrome removal |
| Drop caps distort per-page medians | ignore outlier glyph heights when computing statistics; merge a lone leading large glyph into the following word |
| LLM damages text | drift guard + full diff report; `--llm never` always available |
| Full run is slow | disk cache makes re-runs free; `--pages` for development |
| Page breaks lost in MOBI | verified and documented; EPUB is the fidelity target |

**Genuinely open, needs a decision:**

1. **Kindle page fidelity.** Accept the loss in MOBI, or add Kindle Previewer 3
   (not installed) to get proper `page-list` support? Recommendation: ship EPUB
   as primary, revisit only if Kindle page numbers actually matter to you.
2. **Italics.** pdfium exposes font names, so `<em>` recovery is feasible on the
   text path but not from tesseract TSV. Out of scope for v1 — the asymmetry
   would make the two paths diverge.
3. **`--llm always` on the Jane Corry text layer.** The typos are real and the
   model fixes some of them, but it also misses some and occasionally drifts.
   This is a quality-versus-risk judgement that is yours to make; the drift
   guard and diff report exist so you can make it with evidence.

**Deliberately out of scope for v1:** images and figures (the stated
requirement is text only), tables, footnotes, and multi-language documents where
the language changes mid-book.
