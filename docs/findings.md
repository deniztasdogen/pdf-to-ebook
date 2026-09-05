# Findings

Date: 2026-09-04. Every number below came from running the real thing on this
machine (macOS arm64, Darwin 25.6.0) against the files in `test/`. Where
something was **not** verified, it says so.

Sections 1-5 are what the initial investigation found. Section 7 is what only
surfaced once the code ran against the whole corpus — those are the expensive
ones, and each has a regression test.

**Update 2026-09-05: `Jane Corry - Kocamın Karısı.pdf`, the two 11-page markdown
extracts of it and all of `test/groundtruth/` were removed from the repo,
deliberately.** Every number taken on them is left standing below, because this
document is a record of runs that happened — but none of those runs can be
repeated here without a local copy of the book. `test/README.md` says what went
and which of it can be rebuilt from the fixtures that remain. The other seven
PDFs are untouched.

---

## 1. The three test files are three genuinely different problems

| File | Pages | Text layer | Layout | Role in the plan |
|---|---|---|---|---|
| `dummy_1.pdf` | 1 | Yes, born-digital, 14 chars | trivial | smoke test for the text path |
| `bookSample.pdf` | 1 | **None** (0 chars, 1 image) | **2 columns + app UI chrome** | the OCR path + column detection |
| `Jane Corry - Kocamın Karısı.pdf` | 419 | Yes, but OCR-derived | 1 column | the full pipeline (removed 2026-09-05) |

Four more files were added later, because the supplied three left real gaps —
no born-digital book with chapters, and no multi-page document without a text
layer. See `test/README.md` for provenance. The most useful addition is
`lady-susan-scanned.pdf`: pages 2-9 of the born-digital `lady-susan.pdf`
rendered to 300 dpi images and rewrapped as an image-only PDF, so the correct
text is known exactly and OCR accuracy becomes a number instead of an
impression.

### `bookSample.pdf` is not a book page

Rendered at 300 dpi and inspected, it is a **screenshot of an e-reader app**
showing a Dutch edition of *Harry Potter en de Geheime Kamer*. It contains:

- two text columns,
- a top UI bar (`< Back to store`, the book title, three icons),
- a bottom progress bar (`Location 5 of 139 • 0%`),
- circular page-nav arrows in the left and right margins,
- a drop cap (`V` of "Voor de zoveelste keer"), and an italic word (`ik`).

So this file exercises OCR, multi-column ordering, and chrome stripping at once.
It is Dutch, not Turkish — the OCR language must be a parameter.

### `Jane Corry` already has a good text layer — OCR is *not* the main path

Metadata says it plainly:

```
/Producer: Adobe Acrobat Pro 11.0.0 Paper Capture Plug-in with ClearScan
```

This is a scan that Acrobat OCR'd with **ClearScan**, which synthesises a
custom CFF font from the scanned glyph shapes and embeds real text with
`ToUnicode` CMaps. Verified: 199 `Type0` / `Identity-H` / `CIDFontType0` fonts,
each with `FontFile3` and a `ToUnicode`.

Full-document survey (0.83 s for all 419 pages):

```
pages=419  total_chars=697,359
empty pages (2):        [7, 418]
low-text pages (<200):  [(0,118), (2,42), (4,30)]     # cover / title / imprint
pages with images (310) # but they still carry 1,400-2,000 chars of good text
```

**Consequence:** for this book the text path covers ~99% of pages. OCR is a
per-page fallback for 5 pages, all front/back matter. The user's stated rule
("if text is available use it, else OCR") is right, but it must be decided
**per page**, not per document.

**Second consequence:** the text is OCR output, so it contains OCR typos even
though no OCR runs in our pipeline. Real examples pulled from the file:

| Extracted | Should be |
|---|---|
| `değil aina anlattıklarına` | `ama` |
| `Sessiziilde karşılık vermeyi` | `Sessizlikle` |
| `geç· saatiere kadar` | `geç saatlere` |
| `Onu sudan· çıkarmaya` | `sudan` (stray interpunct) |
| `onunitini değil` | joined words |
| `Savunma avukatını abmağın biriydi:'` | garbled + wrong quote |
| `Ed' in kolu`, `güldür üyor` | spurious intra-word spaces |

So the LLM proofread pass is valuable on the **text** path too, not only after
OCR. The plan makes it a separate switch.

---

## 2. Library landscape: Rust is enough. No hand-written C++ needed.

Verified by actually building and running, not by reading docs.

### PDF text + rasterisation: `pdfium-render`

`pdfium-render 0.9.3` + `pdfium-bundled 0.1.1` (auto-downloads and caches
`libpdfium.dylib`) compiled and ran first try on arm64 macOS. It gives
**everything the pipeline needs from one dependency**:

- per-character Unicode, `loose_bounds()`, `tight_bounds()`, `scaled_font_size()`
- page object types (so images can be counted and ignored)
- page rasterisation: a 300 dpi render of a 841×595 pt page (3507×2480 px) took **70 ms**
- whole-document text extraction: 419 pages in **0.83 s**

Licence is MIT/Apache-2.0; pdfium itself is BSD-3. Alternatives considered:
`mupdf` (AGPL, builds MuPDF from source), `poppler-rs` (GPL), `pdf-extract`
(pure Rust but no rasteriser and weaker on CID fonts). pdfium wins because it
is the only one that does **both** text geometry and rendering.

### OCR: Tesseract. **`ocrs` is disqualified.**

`ocrs 0.13` is the attractive pure-Rust option, but its recognition model's
alphabet is hardcoded ASCII (`src/lib.rs:34`):

```
" 0123456789!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~EABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
```

No `ı ğ ş ç ö ü İ` (Turkish) and no `ë ï é` (Dutch). The alphabet is tied to the
trained model, so it cannot simply be extended. **`ocrs` cannot do either of our
languages.**

Tesseract 5.5.3 installed via Homebrew (`tesseract` + `tesseract-lang`, 163
languages including `tur` and `nld`). On the two-column Dutch screenshot:

```
tesseract booksample_300.png stdout -l nld --psm 1     # 1.0 s
```

It ordered the two columns correctly and read the Dutch cleanly, including
curly quotes and diacritics. It also picked up the UI chrome, which confirms
chrome stripping is needed.

Critically, `tsv` output gives the geometry the pipeline needs:

```
level page_num block_num par_num line_num word_num left top width height conf text
5     1        6         1       3        1        854  1355 99    36     96.5 door
```

Word boxes, block/paragraph/line structure, and **per-word confidence** — the
last one lets us send only doubtful paragraphs to the LLM.

For v1, shell out to the `tesseract` CLI and parse TSV (zero build risk, already
installed). Keep it behind a trait so `leptess` (FFI, no process spawn) can
replace it later. I did **not** verify a `leptess` build against Homebrew's
leptonica.

### EPUB: hand-rolled. MOBI: calibre.

I hand-built a minimal EPUB 3 (with a `page-list` nav and Turkish text) and
converted it:

```
ebook-convert book.epub book.azw3   # OK  (calibre 8.16.2, already installed)
ebook-convert book.epub book.mobi   # OK
ebook-convert book.azw3 back.txt    # Turkish çğıöşü survived intact
```

There is **no usable native Rust MOBI writer**. That is the one place we depend
on an external tool. `epub-builder 0.8.3` exists but does not model the EPUB 3
`page-list` nav, so writing the EPUB ourselves with the `zip` crate is simpler
and I have already verified the exact file layout that calibre accepts
(`mimetype` stored uncompressed first, then `META-INF/`, then `OEBPS/`).

---

## 3. Geometry details that will otherwise cost days

These are the non-obvious traps. Each one is verified.

### 3.1 Use `loose_bounds()`, never `tight_bounds()`

`tight_bounds()` is the ink box. Grouping characters into lines by it shatters
every line that contains a descender or a Turkish diacritic — `y`, `ğ`, `ş` and
quote marks each land in their own pseudo-line:

```
[ 2] "duğunu haal edemiorum. Onu sudan çıkarmaa çalışırken ben de"
[ 3] "yyy"                     <-- the descenders of the line above
[ 5] "bilmiordum doru ei aı amadıımı bile bilmiordum. Bu"
[ 6] "y,ğşyyppypğy"            <-- and again
```

Switching to `loose_bounds()` (the font/text-run box) produced clean lines
immediately. This single detail is the difference between a working and a
useless text path.

### 3.2 pdfium emits synthetic `\r\n` characters at font size 1.0

They appear in the char stream with `scaled_font_size() == 1.0` and a
degenerate box. They must be filtered before computing any geometry, or they
poison the line and margin statistics.

### 3.3 The hyphenation marker in this file is `U+0002`

Not `U+00AD`. Verified across pages — a line ending mid-word carries `U+0002`
and the word continues on the next line:

```
[ 5] "... bile bilmiyordum. Bu\u{2}"
[ 7] "nun üzerine 999'u aradım:'"        -> "bilmiyordum. Bunun üzerine ..."
[28] "... \"Durumu yanlış anladı\u{2}"
[30] "lar. Savunma avukatını ..."        -> "anladılar."
```

(`pypdf` reports the same positions as `U+00AD`, so handle both, plus a literal
`-`.) Only 16 `U+00AD` remain document-wide, so `U+0002` is the real signal.

### 3.4 Every page has a different size and a different margin

Page dimensions across the 419 pages range from `474x788` to `545x866` pt, with
over 200 distinct sizes — each scanned page was cropped slightly differently.

The paragraph indent therefore **cannot** be a constant:

| Page | body margin (continuation lines) | first-line indent |
|---|---|---|
| 40 | x ≈ 31 | x ≈ 54 |
| 300 | x ≈ 28.5 | x ≈ 47 |

Both the header/footer bands and the indent threshold must be derived
**per page**, from that page's own statistics, in units normalised to the page
box. This is the single biggest structural constraint on the design.

### 3.5 First-line indent is a strong, clean paragraph signal

Once normalised per page it is very reliable. Page 40, `loose_bounds()`:

```
[ 9] x=[ 53.78..468.70] "Son bölümü sakin, ifadesiz bir ses tonuyla söylüyor. ..."  <- indented: NEW
[11] x=[ 31.96..468.68] "değil aina anlattıklarına yabancılaşmış gibi bir hali ..."  <- flush: continuation
[13] x=[ 31.91..274.22] "hakim olmaya çalışan birine benziyor."                      <- short: para ends
[15] x=[ 54.00..465.83] "\"Polisler eve geldiklerinde senin fazla sakin ..."         <- indented: NEW
```

Combined with "previous line ends well short of the right edge", this covers
paragraph detection without needing font metrics.

### 3.6 Scan skew splits lines — y-tolerance must be adaptive

With a fixed 4.0 pt tolerance, page 300 produced a spurious line:

```
[11] y=536.86 x=[ 54.25..451.74] "-ağzına çok yakın bir yerden-öpmesi. Carla'nın fazla uğraşma\u{2}"
[12] y=531.56 x=[ 28.81.. 53.89] "dan "        <-- same visual line, 5.3 pt lower
```

`dan` belongs at the *start* of line 11. So: cluster by y with a tolerance
proportional to the median glyph height, then sort fragments by x within the
line.

### 3.7 Running header and printed page number are both present

Every body page starts with a running header and ends with a page number:

```
[ 0] y=760.36 sz=12.70 "JANE CORRY ll KOCAMIN KARISI"     # top band
[62] y= 15.90 sz=11.00 "·41 -"                            # bottom band, PDF index 40
```

Two things follow. The header must be stripped, and the footer is the
**printed** page label — which is offset from the PDF index (index 40 → printed
41). The PDF's own page labels are just `1,2,3,...` so they are no help.

### 3.8 Font size is unreliable for heading detection

ClearScan rescales synthesised glyphs per line. On page 300 alone the body text
reports sizes 13.15, 14.85 and 13.90, while the chapter heading reports 10.80 —
**smaller than the body**:

```
[ 0] sz=10.80 "JANE CORRY ll KOCAMIN KARISI"   # running header
[ 1] sz=10.80 "44"                             # chapter number, centred
[ 2] sz=12.45 "Carla"                          # POV name, centred
[ 3] sz=13.15 "Tabii ki yeni resmin tanıtımı..."  # body
```

So heading detection must weight **centring and vertical isolation** above font
size.

### 3.9 The PDF outline is useless for chapters

419 bookmarks, one per scanned image:

```
0 'kocamın karısı - 0001'
1 'kocamın karısı - 0002'
...
```

Chapter structure has to be recovered from the page geometry.

---

## 4. Ollama / `gemma4:e4b` behaviour

The model exists locally (`gemma4:e4b`, 9.6 GB, Q4_K_M, 8B) and the server
answers on `http://localhost:11434`.

### 4.1 It is a thinking model — you must send `"think": false`

Without it, the answer goes into the `thinking` field and `message.content`
comes back **empty**. 2 of 5 test paragraphs returned nothing, and latency was
6–17 s each. With `"think": false`:

| | latency | empty responses |
|---|---|---|
| default (thinking on) | 5.7–17.3 s | 2 / 5 |
| `"think": false` | 0.8–1.3 s | 0 / 5 |

An 8x speedup and the failure mode disappears.

### 4.2 Batching helps but silently drops paragraphs

Ten paragraphs in one `format: json` request: 4.6 s total = **0.46 s/paragraph**
(2.4x better than one-at-a-time). But it returned **9 items for 10 inputs**,
silently omitting the shortest one (`"Aslında haklı."`).

So batching is worth it, but the response length must be validated and the
batch re-run one-by-one on mismatch.

### 4.3 Concurrency does not help — it is GPU-bound

```
concurrency=1: 0.89 req/s
concurrency=4: 1.26 req/s
concurrency=8: 1.31 req/s
```

Past ~4 in flight there is nothing to gain. Budget from the batched figure
instead.

### 4.4 Quality is useful but imperfect — it needs a guard

Real corrections it got right:

```
"Sessiziilde karşılık vermeyi"  -> "Sessizlikle karşılık vermeyi"   correct
"Onu sudan· çıkarmaya"          -> "Onu sudan çıkarmaya"            correct
"...biriydi:'"                  -> "...biriydi:\""                  correct
```

And the failures:

```
"değil aina anlattıklarına"     -> unchanged                 missed a real typo
"geç· saatiere kadar"           -> "geç saate kadar"          DRIFT: dropped a suffix, changed meaning
"onunitini değil"               -> "onun ünitesini değil"     plausible-looking but wrong
```

The `saatiere -> saate` case is the dangerous one: it is not a typo fix, it is a
content change. So every correction must pass an edit-distance guard, and
anything that drifts too far is rejected in favour of the original.

### 4.5 Proofreading quality, measured (2026-09-05)

§4.4 judged the model by reading its output. This measures it. Two fixtures,
each 11 pages of the Turkish book, each with a human-grade reference proofread
in `test/groundtruth/`; the score is character-level edit distance from the
reference. **`p40-50` is the set the prompt was tuned on and `p100-110` was held
out** — it was never looked at until the prompt was finished.

Neither fixture nor either reference is in the repo any more (see
`test/README.md`). The tables below are the record of that run; re-scoring a new
prompt means building an equivalent pair first, and the lesson under them is why
it has to be a *pair*.

Tuned set, `test/kocamin-karisi-p40-50.md`, 140 spans, raw OCR **1.67%** CER:

| Prompt | CER | error reduction | fixed | missed | overreach |
|---|---|---|---|---|---|
| v1 — "fix OCR errors", ratio-only guard | 1.18% | +29.5% | 10 | 12 | 4 |
| v2 — ranked artefact classes | 1.19% | +28.9% | 21 | 6 | **8** |
| v3 — fragment rule first, + boundary guard | 1.01% | +39.3% | 23 | 7 | 2 |
| v4 — + colon, lost-`ğ`, apostrophe, `İ` | 0.83% | +50.5% | 34 | 8 | 2 |
| **v5 — `�` rule generalised** | **0.88%** | **+47.5%** | **36** | 8 | 2 |

Held-out set, `test/kocamin-karisi-p100-110.md`, 127 spans, raw OCR **1.55%**:

| Prompt | CER | error reduction | fixed | missed | overreach |
|---|---|---|---|---|---|
| v1 | 1.00% | +35.2% | 15 | 15 | 2 |
| v4 | 1.00% | +35.2% | 27 | 14 | 1 |
| **v5** | **0.94%** | **+39.0%** | **34** | 14 | **0** |

**Run the held-out set. It changed the answer.** On the tuned pages v4 looked
like the winner at 0.83% CER. On pages it had never seen, v4's CER advantage
vanished entirely — identical to v1 at 1.00% — because one of its rules was
overfitted: v4 asserted that `�` marks a lost `ğ`, which is true on p40-50 and
false on p100-110, where `�` stood for a full stop, a `d`, an `l`, an `m` and an
`ıkar`. On three of the five held-out spans containing `�`, **v4 was worse than
v1**: `Haline` was left broken, `çıkardığının` became `çdığının`, `kolu` became
`kökü`. v5 replaces the claim with "`�` is one character that failed to decode,
work out which from the word", and is then better than v1 on both sets. The
0.83% -> 0.88% regression on the tuned set is the price of that, and worth it.

The class distribution is not stable between extracts either, which is why the
prompt lists classes rather than encoding them as rules: mangled quotes lead
both sets, but proper-noun apostrophes are second on p100-110 (~19) and eighth
on p40-50, and `-ıyar` for `-ıyor` does not occur on p100-110 at all.

Four things this showed that reading the output had not:

**The prompt was the whole problem, not the model.** Same model, same 140 spans:
correct fixes went from 10 to 36 on the tuned set and from 15 to 34 on the
held-out one, and overreach fell to 2 and 0. Nothing about `gemma4:e4b`
changed — only what it was told and what the guard would accept.

**The largest artefact class was never in the prompt.** Mangled quotation marks
are ~57 of the 172 repairs these pages need — a third of all the work — and v1
said nothing about quotes. Worse, the model's instinct was wrong in a specific
way: the scan renders a closing `."` as `:'`, and the model kept the colon,
emitting `girdim:"`. Naming the colon as a misread full stop is what fixed it:

```
                    :'      :"  (wrong)     ."  (right)
raw scan            44       0               6
v1                   6      36              10
v3                   0      28              28
v4                   1      14              44
reference            0       0              55
```

**A prompt rule stated after the rule it contradicts does not hold.** v2 halved
the misses and doubled the overreach, for no net gain. The cause: "do not add
text" was rule 8, *after* "close the quotation" was rule 1, so the model
appended a second `?` to spans already ending `?"` and completed words cut off
by a page break. Moving it to rule 0 was not enough on its own — `drift_reason`
now also refuses any correction that is a pure addition at the start or end,
because a span is often a page-break fragment and completing it is never a
repair. Prompt and guard together took overreach from 8 to 2.

**The ratio guard was rejecting short spans as a class.** Over the full book the
median refused span was 36 characters against 131 for accepted ones. On a
14-character string a correct 2-character quote repair is 86% similar, so both
ratios refuse it; `"Emin misin?''` -> `"Emin misin?"` and `Kocam1n Kar1s1` ->
`Kocamın Karısı` were both thrown away. `ALWAYS_ALLOWED_EDITS = 4` is an
absolute floor under the ratios.

Two smaller things, both real:

- **A truncated reply leaked its own JSON envelope into the prose.** 10 replies
  in the 419-page book were cut off by `num_predict`; `parse_reply` failed to
  parse them and fell back to "treat the whole reply as one paragraph", which
  spliced a literal `{"paragraphs":[` into the document. The drift guard caught
  all ten, but only by accident of their length. Broken JSON is now refused.
- **Naming the language in the prompt costs nothing.** `core::label_for` turns
  `tur` into `Turkish`; an unknown tesseract code yields `None` and the sentence
  is left out rather than guessed.

What is still wrong at v5, and is not worth chasing with this model:

- Spans still ending `:"` rather than `."`. A blunt `:"` -> `."` rewrite would
  fix all of them and would also damage legitimate Turkish
  (`Şöyle dedi: "Merhaba"`), so it is not safe as a deterministic rule.
- Substitutions still slip through on long spans, where 10% similarity is a wide
  budget: `mosmor` ("bruised") became `mızmor`, which is not a word.
- Both references have judgement calls of their own — 14 of 172 changes on
  p40-50 and 17 of 123 on p100-110 were flagged low-confidence, e.g.
  `Saralı'nın` -> `Sarah'ın`. So the CER floor is not zero and small gaps are
  noise. The tuned-set 1.18% -> 0.88% and held-out 1.00% -> 0.94% are the
  numbers to quote; treat anything under ~0.05% as a tie.

### 4.6 The prompt split by language (2026-09-05, not measured)

v5 was one prompt. Three of its fourteen rules were pure Turkish orthography —
lost `ğ`, sentence-initial `İ`, the apostrophe before a case suffix — a fourth
was half Turkish (`ın`/`nı` for `m`, `rı` collapsed to `n`), and every worked
example in the list was a Turkish word. An English or Dutch book was told all of
it, under a heading that correctly said the text was in English.

v6 splits it in two — a general prompt in `proofread/mod.rs` and a per-language
one in `proofread/language.rs`, selected by the tesseract code from `--lang` or
the GUI. Turkish is the only pack, and it keeps every rule and every example v5
had; the general rules keep their classes with English examples in place of the
Turkish ones they were derived from.

**This is a refactor, not a measured improvement, and it was not re-scored.**
The tables above are v5's. Two things could move the Turkish number and neither
has been checked against ollama:

- The Turkish rules now sit at 9–13 rather than 3–12, after the general classes
  instead of interleaved with them. Rule 0 is still first and the limits are
  still last, which are the two orderings §4.5 did measure.
- The Turkish examples for the general classes are now one line at the end of
  the pack rather than attached to the rule each belongs to.

Re-run both fixtures before quoting a CER for v6. The claim that holds without a
run is the narrow one: a non-Turkish book no longer receives Turkish
orthography rules.

---

## 5. Output-stage caveat: page breaks do not survive MOBI

Verified with the hand-built EPUB. Turkish text survived EPUB → AZW3 → text
perfectly. But the page anchors did not:

```
strings book.azw3 | grep -ci 'page_4|pagebreak'   ->  0
```

calibre's KF8 writer drops `<span epub:type="pagebreak">` anchors and the
`page-list` nav. So:

- **EPUB 3 is the format that preserves page breaks.** Make it the primary target.
- For Kindle, offer an explicit `--page-breaks hard` mode (CSS
  `page-break-before: always`), understanding that it fights reflow.
- Kindle Previewer 3 honours EPUB 3 `page-list` properly, but it is **not
  installed** here, so I could not verify that route.

## 6. Tooling present vs missing

| Tool | Status |
|---|---|
| Rust 1.94.1 / cargo 1.94.1 | present |
| pdfium (via `pdfium-bundled`) | downloads and caches automatically |
| tesseract 5.5.3 + 163 langs (`tur`, `nld`) | installed during this investigation |
| calibre 8.16.2 `ebook-convert` | present at `/Applications/calibre.app/Contents/MacOS/ebook-convert` |
| ollama + `gemma4:e4b` | present and responding |
| `epubcheck` | **missing** — worth adding for EPUB validation in tests |
| Kindle Previewer 3 | **missing** |

---

## 7. What only showed up once the code ran

Sections 1-6 came from probing. Everything below came from running the pipeline
over the whole corpus and comparing the output against a known-good reference.
Each item has a regression test named after it.

### 7.1 ClearScan reports one box for a whole glyph cluster

The single most destructive finding. pdfium sometimes returns the **same**
bounding box for consecutive characters, and sometimes one box covering an
entire cluster:

```
[47] 'R' x=[ 50.88.. 60.83]
[48] 'a' x=[ 60.67.. 76.05]
[49] 'h' x=[ 60.67.. 76.05]   <- same box as the 'a'
...
'k' of "kınklığı" (page 293)  x=[310.35..368.67]   <- 58pt wide, one glyph
```

Grouping glyphs into words by measuring the gap from the previous glyph's right
edge therefore sees a jump backwards of tens of points inside ordinary words.
With a one-em tolerance this split nearly every word in the book:

```
Ra hatlıyorum. Tam zama nında ...      JA NE COR RY ll KOCA MIN KARISI
k ı n k l ı ğ ıa u ğ r a d ı ğ ı       Sa na k a lmış
```

The fix is to measure the backward jump from the **word's** left edge rather
than the previous glyph's right edge. A genuine jump back — returning to the
left column of a spread — still lands well before the word's own start.

Word-sequence fidelity against pdfium's own text went from 98.29% to 99.17%,
and the output stopped being 2,201 words longer than the source.

### 7.2 Word splitting must follow the PDF's spaces, not geometry

Related but separate. pdfium already emits space characters and knows where the
words break; a geometric gap threshold tight enough to catch a *missing* space
also fires inside words.

The geometric fallback is still needed — the second column of
`under-lock-and-key.pdf` lost almost all of its space characters during the
original OCR (`entvoiceagain.“And,ifyoutakemyad¬`). So it is now switched on per
page, only when a page turns out to have fewer than one space per twelve
characters. Running prose has roughly one per five or six.

### 7.3 A real space can be reported with font size 1.0

The space in "Mr. Vernon." on page 2 of `lady-susan.pdf`:

```
[39] '.' U+002E sz=9.96
[40] ' ' U+0020 sz=1.00     <- a real space
[41] 'V' U+0056 sz=9.96
```

pdfium uses font size 1.0 for the synthetic `\r\n` it inserts between lines,
which have to be filtered out. Filtering on size *before* recognising
whitespace also removed this space, welding the words into `Mr.Vernon.`. Order
matters: whitespace first, then the size filter.

### 7.4 Vertical bands must be normalised by font size

ClearScan's synthetic fonts report boxes wildly out of proportion to their
nominal size. On the chapter opener of page 66 a 13.9pt glyph comes back 42pt
tall, which overlaps the chapter number on the line above:

```
"8"   y=[166.52..208.77] size=13.90
"C"   y=[211.13..227.34] size=15.60
"ar"  y=[190.76..233.01] size=13.90
```

Judging line membership on those boxes merged the two lines and x-sorted them
into `C ar 8 la`. Deriving the band from the font size around the box centre
fixes it, and leaves body text alone — there the box is already about 1.25x the
font size.

The band also has to be **capped at the page's median** font size. The box rule
around the newspaper clipping on page 90 is set in 23.75pt against 17pt body
text; uncapped, its band reached the text lines both above and below it and
chained them into one, interleaving two whole lines of the article.

### 7.5 Phantom glyphs with no area

Page 66 carries a `ka` whose box is `x=[359.32..359.32], y=[92.78..92.79]` —
nothing is drawn. Left in, it lands in the middle of a real word:
`not dik ka timi dağıttı`. Glyphs with no width or no height are now dropped.

### 7.6 A nanosecond timestamp is not a unique filename

The OCR path writes each rendered page to a temp file before calling
`tesseract`. Naming it from `SystemTime::now().as_nanos()` looked safe and was
not: pages are recognised in parallel, threads that start together can read the
same nanosecond, and one page then overwrites another's image.

The symptom was not a crash. The book came out with a page duplicated and a page
missing, and word similarity against ground truth sat at 74.78% with content
appearing in the wrong order. With an atomic counter instead it is **99.26%**.

Worth recording because nothing about the failure pointed at file naming.

### 7.7 Column gutters are sized in ems, not page fractions

Two failed approaches before the working one:

* Measuring strip occupancy by **accumulated glyph height** made a strip
  holding a single line of text look empty, inventing gutters in sparse regions.
* Sizing the gutter as a **fraction of page width** rejected the real thing. The
  CVPR gutter in `resnet-two-column.pdf` is about 17pt on a 495pt body — under
  4%. Measured against the 10pt glyphs it is 1.7x, and an inter-word space is
  about 0.25x, so the font size separates them with room to spare.

The working detector quantises words into text rows and asks what share of rows
reach into each vertical strip. A gutter is a strip at least 1.5x the median
glyph height wide that at most 15% of rows touch. The 15% is not zero because a
full-width figure crosses the gutter for the rows it spans.

### 7.8 Column count is a document property

Even with a good detector, page 4 of `resnet-two-column.pdf` came out with
**four** columns: it holds a table whose internal whitespace reads as two extra
gutters, and its text was interleaved.

Settling the column count across all pages first and capping each page at the
document's own count fixes it. A page that finds *fewer* gutters is left alone,
because a full-width table genuinely has none.

### 7.9 Running headers need fuzzy matching

Exact signature matching removed the Turkish book's running header from about
300 pages and left it on 116. The header is not extracted consistently:

```
JANE CORRY ll KOCAMIN KARISI
JANE CORRY // KOCAMIN KARISI 41     <- separator read differently
JANE COR RY ll KOCAMIN KARISI       <- word break in a different place
JA.NH COIU?Y ll KOCAMIN KARISI      <- badly mangled
```

Stripping digits and punctuation is not enough, because `//` disappears
entirely while `ll` does not. Dropping whitespace as well makes the third form
match, and comparing signatures by normalised edit distance (≥85%) absorbs the
rest. Survivors: 116 → 1, the badly mangled one.

### 7.10 Band membership must use the leading edge

The header bar of `bookSample.pdf` starts 43pt down a 595pt page, comfortably
inside a 10% band, but its tallest glyph reaches 54.2pt — so testing the
*bottom* edge put it 0.6pt outside the band and treated the whole thing as body
text. Testing the top edge for the top band (and the bottom edge for the bottom
band) is both more correct and less brittle.

### 7.11 Screenshot chrome needs two more rules

Neither repetition (one page) nor a gap threshold (the bar sits 25pt above the
body, closer than two lines of text) nor the page's one horizontal rule (it is
*above* the bar, not below) could identify the e-reader UI. Two structural rules
did:

* A band line that **crosses a column gutter** is not body text — body lines
  stay inside their column. This catches the centred title bar.
* A band line lying **entirely outside the body's left/right extent** is
  furniture in the margin. This catches the `< Back to store` button.

### 7.12 Section headings in non-fiction are bold, not big

Every section heading in `attention-is-all-you-need.pdf` is 9.96pt bold against
9.96pt roman body text. No size or position test can find them, so the text path
now records the font weight from pdfium and treats a short, flush-left, bold,
vertically isolated line as a heading. Headings found on that paper went from 12
to 41.

Isolation on both sides is required, so a bold lead-in phrase inside a paragraph
("**Encoder:** The encoder is composed of…") is not promoted.

---

## 8. Measured quality

| Check | Result |
|---|---|
| OCR accuracy on `lady-susan-scanned.pdf` vs known-good text | **99.26% word**, 99.86% character |
| Turkish book vs pdfium's own text, headers and page numbers removed | **99.17% word** (91,280 vs 91,178 words) |
| Turkish book, 419 pages, text + layout + EPUB | **6.7s** |
| OCR, 8 pages at 300 dpi, parallel | **1.7s** |
| LLM proofreading, long paragraphs | ~1.8-2.5s each; 12 of 13 fixes accepted, 0 refused |
| Unit and integration tests | **91 passing** |

Remaining differences on the Turkish book are mostly reordering of garbled
ClearScan fragments, where the geometric reading order is arguably more correct
than pdfium's content-stream order.

## 9. Known limitations

* **Figure labels leak into academic output.** `x`, `weight layer` and similar
  appear as paragraphs in `resnet-two-column.pdf`. They are real text in the
  PDF, drawn as vector graphics rather than an image, so "ignore images" does
  not reach them. Harmless for books, which is the target.
* **Headings are not proofread.** The LLM pass rewrites paragraphs only, so
  `## C ar la` stays — ClearScan put real spaces inside that word.
* **Italics are dropped.** pdfium exposes `font_is_italic`, but tesseract does
  not, so using it would make the two paths diverge. The bigger obstacle is the
  proofreading stage, which rewrites one text span per paragraph and would have
  to be reworked to preserve inline runs.
* **`epubcheck` is not installed**, so "valid EPUB" is checked structurally
  (mimetype first and stored, container, OPF, nav, page-list) rather than by a
  validator.
* **The GUI was not visually verified.** It builds, launches and stays up, but
  this environment denies screen recording, so no screenshot was taken.
