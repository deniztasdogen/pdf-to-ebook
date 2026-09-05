//! The run report: what was dropped, what the model changed, what to check.

use crate::Outcome;
use pdftomobi_core::{Config, InputKind};
use std::fmt::Write;

pub fn render(cfg: &Config, o: &Outcome) -> String {
    // A markdown input never touched extraction: page counts and dropped chrome
    // have nothing to say, so those sections are left out rather than printed as
    // rows of zeroes. Proofreading is not one of them — it can run over a
    // markdown input — so that section follows the model, not the input kind.
    let from_pdf = cfg.input_kind() != Some(InputKind::Markdown);

    let mut s = String::new();
    let _ = writeln!(s, "# Conversion report\n");
    let _ = writeln!(s, "- Input: `{}`", cfg.input.display());
    if from_pdf {
        let _ = writeln!(s, "- Language: `{}` ({})", cfg.lang, cfg.bcp47());
    } else {
        let _ = writeln!(s, "- Language: `{}`", o.document.meta.language);
    }
    let _ = writeln!(s, "- Page breaks: {:?}", cfg.page_breaks);
    if let Some(p) = &o.proofread_path {
        let _ = writeln!(
            s,
            "- Proofread markdown: `{}` — the ebook was built from this file, \
             not from the one the model was given",
            p.display()
        );
    }
    let _ = writeln!(s);

    if from_pdf {
        let _ = writeln!(s, "## Pages\n");
        let _ = writeln!(s, "| | count |");
        let _ = writeln!(s, "|---|---|");
        let _ = writeln!(s, "| used the PDF text layer | {} |", o.text_pages);
        let _ = writeln!(s, "| needed OCR | {} |", o.ocr_pages);
        let _ = writeln!(s, "| produced nothing | {} |", o.empty_pages.len());
        let _ = writeln!(s);
        if !o.empty_pages.is_empty() {
            let list: Vec<String> = o
                .empty_pages
                .iter()
                .take(40)
                .map(|p| (p + 1).to_string())
                .collect();
            let _ = writeln!(
                s,
                "Empty pages (1-based): {}{}\n",
                list.join(", "),
                if o.empty_pages.len() > 40 { " …" } else { "" }
            );
        }
    }

    let _ = writeln!(s, "## Result\n");
    let _ = writeln!(s, "- Paragraphs: {}", o.paragraphs);
    let _ = writeln!(s, "- Headings: {}", o.headings);
    let _ = writeln!(s, "- Page markers: {}", o.page_markers);
    let _ = writeln!(s, "- Characters: {}", o.characters);
    let _ = writeln!(s);

    if from_pdf && !o.dropped.is_empty() {
        let _ = writeln!(s, "## Removed as headers, footers or chrome\n");
        let _ = writeln!(
            s,
            "Nothing here is in the output. Check the list if text looks missing.\n"
        );
        let _ = writeln!(s, "| text | reason | pages |");
        let _ = writeln!(s, "|---|---|---|");
        for (text, reason, count) in o.dropped.iter().take(30) {
            let _ = writeln!(
                s,
                "| `{}` | {} | {} |",
                text.replace('|', "\\|"),
                reason,
                count
            );
        }
        let _ = writeln!(s);
    }

    if from_pdf || o.llm.considered > 0 {
        let _ = writeln!(s, "## Proofreading\n");
        if o.llm.considered == 0 {
            let _ = writeln!(s, "Not run.\n");
        } else {
            let _ = writeln!(s, "- Paragraphs considered: {}", o.llm.considered);
            let _ = writeln!(s, "- Corrections accepted: {}", o.llm.changed);
            let _ = writeln!(
                s,
                "- Corrections refused by the drift guard: {}",
                o.llm.rejected
            );
            let _ = writeln!(s, "- Served from cache: {}", o.llm.cache_hits);
            // Only worth printing when the work was actually shared out. The
            // split is uneven by design: batches are pulled, not dealt.
            if o.llm.per_server.len() > 1 {
                let split: Vec<String> = o
                    .llm
                    .per_server
                    .iter()
                    .map(|(url, n)| format!("`{url}` {n}"))
                    .collect();
                let _ = writeln!(s, "- Batches per server: {}", split.join(", "));
            }
            if !o.llm.servers_lost.is_empty() {
                let _ = writeln!(
                    s,
                    "- Servers that stopped answering mid-run: {}",
                    o.llm.servers_lost.join(", ")
                );
            }
            if o.llm.unproofread > 0 {
                let _ = writeln!(
                    s,
                    "- Paragraphs left as extracted, because no server was still \
                     answering: {}",
                    o.llm.unproofread
                );
            }
            if o.llm.batches_split > 0 {
                let _ = writeln!(
                    s,
                    "- Batches redone one paragraph at a time, after coming back the \
                     wrong length or failing outright: {}",
                    o.llm.batches_split
                );
            }
            let _ = writeln!(s);

            let accepted: Vec<_> = o.corrections.iter().filter(|c| c.accepted).collect();
            if !accepted.is_empty() {
                let _ = writeln!(s, "### Accepted changes\n");
                for c in accepted.iter().take(80) {
                    let _ = writeln!(
                        s,
                        "- `{}`\n  → `{}`",
                        short(&c.original),
                        short(&c.corrected)
                    );
                }
                if accepted.len() > 80 {
                    let _ = writeln!(s, "\n…and {} more.", accepted.len() - 80);
                }
                let _ = writeln!(s);
            }
            let refused: Vec<_> = o
                .corrections
                .iter()
                .filter(|c| c.reason.is_some() && !c.accepted)
                .collect();
            if !refused.is_empty() {
                let _ = writeln!(s, "### Refused changes\n");
                let _ = writeln!(s, "The original text was kept in every case below.\n");
                for c in refused.iter().take(40) {
                    let _ = writeln!(
                        s,
                        "- `{}` — {}",
                        short(&c.original),
                        c.reason.as_deref().unwrap_or("")
                    );
                }
                let _ = writeln!(s);
            }
        }
    }

    if !o.ambiguous_joins.is_empty() {
        let _ = writeln!(s, "## Ambiguous hyphen joins\n");
        let _ = writeln!(
            s,
            "A literal `-` at a line end was treated as hyphenation and the word \
             was welded. Check these if a compound word looks wrong.\n"
        );
        for j in o.ambiguous_joins.iter().take(40) {
            let _ = writeln!(s, "- `{j}`");
        }
        if o.ambiguous_joins.len() > 40 {
            let _ = writeln!(s, "\n…and {} more.", o.ambiguous_joins.len() - 40);
        }
        let _ = writeln!(s);
    }

    if !o.warnings.is_empty() {
        let _ = writeln!(s, "## Warnings\n");
        for w in &o.warnings {
            let _ = writeln!(s, "- {w}");
        }
        let _ = writeln!(s);
    }

    let _ = writeln!(s, "## Files written\n");
    for p in &o.written {
        let _ = writeln!(s, "- `{}`", p.display());
    }
    s
}

fn short(s: &str) -> String {
    let t = s.replace('`', "'").replace('\n', " ");
    if t.chars().count() <= 110 {
        t
    } else {
        let head: String = t.chars().take(107).collect();
        format!("{head}…")
    }
}
