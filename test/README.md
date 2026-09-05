# Test fixtures

Added 2026-09-04. Each file exercises a different failure mode. See
`docs/findings.md` for what was measured on each.

| File | Pages | Text layer | Layout | Language | What it tests |
|---|---|---|---|---|---|
| `dummy_1.pdf` | 1 | born-digital, 14 chars | trivial | en | smoke test |
| `lady-susan.pdf` | 50 | born-digital, 131k chars | 1 col | en | clean text path, first-line indents, running header, `U+0002` hyphens |
| `attention-is-all-you-need.pdf` | 15 | born-digital, 40k chars | 1 col | en | **bold** section headings, figure text to ignore |
| `resnet-two-column.pdf` | 12 | born-digital, 60k chars | **2 col** | en | narrow (17pt) column gutter, a table that looks like extra gutters |
| `bookSample.pdf` | 1 | **none** | **2 col + app UI chrome** | nl | OCR, column order, chrome stripping, drop cap |
| `lady-susan-scanned.pdf` | 8 | **none** | 1 col | en | **OCR with exact ground truth** (see below) |
| `under-lock-and-key.pdf` | 36 | scan OCR, 155k chars | **2 col**, skewed | en | stress: interleaved columns, `¬` hyphens, missing spaces |

## What was removed on 2026-09-05

The Turkish book and everything cut from it were taken out of the repo
deliberately:

- `Jane Corry - Kocamın Karısı.pdf` — 419 pages, ClearScan text layer, the only
  full-length book here. It was the fixture for per-page size drift, chapter
  detection and end-to-end timing.
- `kocamin-karisi-p40-50.md` and `kocamin-karisi-p100-110.md` — 11-page extracts
  of it, the tuning and held-out sets for proofreading.
- `groundtruth/` in full — see below.

Nothing in the test suite reads any of them: every test in the workspace is a
unit test with its data inline, and `cargo test --workspace` still passes 135
tests. What is gone is the ability to *measure* two things, and both were the
point of having the files:

**Proofreading quality.** `docs/findings.md` §4.5 scores the model as character
error rate against a hand proofread. Rebuilding that needs two extracts and two
references, not one — tune on the first, and keep the second untouched until the
prompt is settled. That is not process for its own sake: a rule tuned on the
first extract (that the replacement character marks a lost `ğ`) was wrong on the
second, where the same character stood for a full stop, a `d`, an `l`, an `m`
and an `ıkar`, and it scored **worse** on three of the five held-out spans that
contained it. On the tuning set alone it had looked like the best prompt yet.
Pick ordinary prose for both, and check that the artefact mix differs between
them — in the old pair, mangled quotation marks led in both, while proper-noun
apostrophes were second in one and eighth in the other.

**OCR accuracy.** See `groundtruth/` below.

Both old references carried the proofreader's own low-confidence calls (14 of
172 changes and 17 of 123), so the CER floor against them was not zero and gaps
under ~0.05% were a tie. Expect the same of any replacement.

If you have a local copy of the book, dropping it back at
`test/Jane Corry - Kocamın Karısı.pdf` restores everything that used it — no code
refers to the path.

## Provenance

- `lady-susan.pdf` — Jane Austen, *Lady Susan*. archive.org item `LadySusan`.
  Public domain.
- `attention-is-all-you-need.pdf` — Vaswani et al., *Attention Is All You Need*,
  arXiv:1706.03762v7. NeurIPS format, which is **single** column — I assumed it
  was two-column when I picked it and had to check. It earns its place anyway:
  its section headings are 9.96pt bold against 9.96pt roman body text, so
  nothing but font weight can find them.
- `resnet-two-column.pdf` — He et al., *Deep Residual Learning for Image
  Recognition*, arXiv:1512.03385v1. CVPR format, genuinely two-column, with a
  gutter of about 17pt on a 495pt body — under 4% of the width, which is what
  broke the first column detector.
- `under-lock-and-key.pdf` — *Under lock and key, or, Marion Marlowe's last
  role* (1901). archive.org item `underlockkeyorma0030shir`. Public domain.
- `lady-susan-scanned.pdf` — **generated**, not downloaded. Pages 2–9 of
  `lady-susan.pdf` rendered to 300 dpi grayscale JPEG and rewrapped as an
  image-only PDF. Verified to contain zero extractable characters.
- `dummy_1.pdf`, `bookSample.pdf` — supplied with the project.

## Why the generated fixture matters

`bookSample.pdf` is the only supplied file with no text layer, and it is a
single page of an e-reader screenshot — too small and too atypical to validate
the OCR path.

`lady-susan-scanned.pdf` fixes that. Because it was rendered *from* a
born-digital PDF, the exact correct text is known: extract it from
`lady-susan.pdf` pages 2–9 and diff against the OCR output. That turns OCR
accuracy into a measurable number instead of a judgement call. A spot check on
page 1 of the fixture already matches the source almost character for character,
including curly quotes and end-of-line hyphens.

## `groundtruth/` — removed, and what it held

The directory went out on 2026-09-05 with the Turkish book. Four of its six
files were derived from that book:

- `kocamin-karisi-p40-50.proofread-reference.md` and
  `kocamin-karisi-p100-110.proofread-reference.md` — hand proofreads of the two
  markdown extracts, structurally identical to them line for line so the two
  could be diffed span by span. The reference for **proofreading** accuracy.
- `kocamin-karisi-p40-50.artefact-classes.md` — the 15 recurring OCR artefact
  classes in the Turkish scan, ranked by frequency, each with examples and a
  prompt rule. `system_prompt` in `crates/extract/src/proofread/mod.rs` is
  derived from this list. **The list itself lives on in the code**; what is gone
  is the evidence behind each rule, so a rule that looks arbitrary there cannot
  be traced back to its examples any more. `docs/findings.md` §4 quotes some.
- `kocamin-karisi-p100-110.notes.md` — the same for the held-out extract, and
  the record of which classes differed.

The other two scored **OCR** accuracy on PDFs that are still here, so they can
be rebuilt:

- `lady-susan-p2-9.lines.txt` — a line-level dump (text plus coordinates) of
  `lady-susan.pdf` pages 2–9, the reference for OCR accuracy on
  `lady-susan-scanned.pdf` and for line-assembly regression tests. The same
  information comes out of `pdf-to-ebook test/lady-susan.pdf --pages 2-9
  --format json`, which dumps the page layout — a different file format, not a
  byte-level restore.
- `under-lock-and-key.archive-ocr.txt` — archive.org's own OCR of that scan, the
  `_djvu.txt` of item `underlockkeyorma0030shir`. Downloadable again from the
  same item. A second opinion, never a gold standard: it has its own errors.

## Note on the two-column files

`bookSample.pdf` (image-only) and `under-lock-and-key.pdf` (a poor scan) were
the only two-column fixtures at first, and neither could tell a column-detection
bug from an OCR problem. `resnet-two-column.pdf` is born-digital, so its
extracted text is exact and any interleaving is unambiguously the detector's
fault. That is how both column bugs in `docs/findings.md` §7.7-7.8 were found.

## Note on `under-lock-and-key.pdf`

Kept deliberately as a stress case, not a target to pass. Its second column
lost most of its spaces during the original OCR (`entvoiceagain.“And,ifyoutakemyad¬`)
and it uses `U+00AC` as the hyphenation marker. No geometric pipeline can repair
missing spaces — only the LLM pass has a chance. Treat regressions here as
informational.
