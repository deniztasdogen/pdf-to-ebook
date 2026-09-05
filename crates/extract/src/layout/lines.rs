//! Grouping words into visual lines.
//!
//! The tolerance has to scale with the text, not be a constant. With a fixed
//! 4pt window, scan skew on page 300 of the Turkish book split the fragment
//! `dan` onto its own line 5.3pt below the rest of its line — and that fragment
//! belonged at the *start* of the line, not the end, so sorting by x within the
//! line matters too.

use crate::geom::median;
use crate::model::{Line, Word};

/// Assemble words into lines.
///
/// Words are matched to a line by vertical overlap rather than by baseline
/// distance, which handles skew and mixed glyph sizes on the same line without
/// tuning. ClearScan output routinely mixes 12.45, 13.15 and 14.85pt glyphs
/// within one page.
pub fn assemble(words: Vec<Word>) -> Vec<Line> {
    if words.is_empty() {
        return Vec::new();
    }
    let heights: Vec<f32> = words.iter().map(|w| w.font_size).collect();
    let med_h = median(&heights).max(1.0);

    let mut sorted = words;
    sorted.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap()
            .then(a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap())
    });

    // Open lines, each a bucket of words plus its running vertical band.
    let mut buckets: Vec<((f32, f32), Vec<Word>)> = Vec::new();

    for w in sorted {
        let ws = w.line_span(med_h);
        let mut target: Option<usize> = None;
        // Walk back over recently opened lines. Anything whose band ends well
        // above this word cannot be its line, so the scan stays short.
        for i in (0..buckets.len()).rev() {
            let (band, _) = &buckets[i];
            if band.1 < ws.0 - med_h * 1.5 {
                break;
            }
            if same_line(*band, ws, med_h) {
                target = Some(i);
                break;
            }
        }
        match target {
            Some(i) => {
                buckets[i].0 = (buckets[i].0 .0.min(ws.0), buckets[i].0 .1.max(ws.1));
                buckets[i].1.push(w);
            }
            None => buckets.push((ws, vec![w])),
        }
    }

    let mut lines: Vec<Line> = buckets
        .into_iter()
        .filter_map(|(_, ws)| Line::from_words(ws))
        .filter(|l| !l.is_blank())
        .collect();
    // Reading order: top to bottom.
    lines.sort_by(|a, b| a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap());
    lines
}

/// Two vertical bands belong to the same line when they overlap by a decent
/// share of the narrower one, or their centres are close relative to the
/// median glyph height.
fn same_line(line: (f32, f32), word: (f32, f32), med_h: f32) -> bool {
    let overlap = line.1.min(word.1) - line.0.max(word.0);
    let narrower = (line.1 - line.0).min(word.1 - word.0).max(0.1);
    if overlap >= narrower * 0.4 {
        return true;
    }
    // Fallback for skew: a small fragment sitting slightly off the line.
    let dc = ((line.0 + line.1) / 2.0 - (word.0 + word.1) / 2.0).abs();
    dc < med_h * 0.55 && overlap > 0.0
}

/// Median vertical distance between consecutive line tops. Used as the unit for
/// "is this gap big enough to mean something".
pub fn median_leading(lines: &[Line]) -> f32 {
    if lines.len() < 2 {
        return lines.first().map(|l| l.bbox.height() * 1.2).unwrap_or(12.0);
    }
    let gaps: Vec<f32> = lines
        .windows(2)
        .map(|w| w[1].bbox.y0 - w[0].bbox.y0)
        .filter(|g| *g > 0.0)
        .collect();
    let m = median(&gaps);
    if m > 0.0 {
        m
    } else {
        12.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;

    fn w(text: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> Word {
        Word {
            text: text.to_string(),
            bbox: Rect::new(x0, y0, x1, y1),
            font_size: y1 - y0,
            conf: None,
            bold: false,
        }
    }

    #[test]
    fn descenders_do_not_split_a_line() {
        // The failure this module exists to prevent: a `y` hanging below the
        // line must not become its own line.
        let words = vec![
            w("hayal", 30.0, 100.0, 60.0, 112.0),
            w("y", 62.0, 103.0, 66.0, 116.0), // descender, sits lower
            w("edemiyorum", 68.0, 100.0, 130.0, 112.0),
        ];
        let lines = assemble(words);
        assert_eq!(lines.len(), 1, "got {:?}", lines.iter().map(|l| l.text()).collect::<Vec<_>>());
    }

    #[test]
    fn skewed_fragment_joins_its_line_and_sorts_by_x() {
        // Page 300 of the Turkish book: `dan` is 5.3pt lower than the rest of
        // its line and starts to the LEFT of it.
        let words = vec![
            w("-agzina", 54.25, 100.0, 120.0, 113.0),
            w("yerden-opmesi.", 122.0, 100.0, 250.0, 113.0),
            w("dan", 28.81, 105.3, 53.89, 118.3),
        ];
        let lines = assemble(words);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].text().starts_with("dan"),
            "fragment must sort to the front, got {:?}",
            lines[0].text()
        );
    }

    #[test]
    fn separate_lines_stay_separate() {
        let words = vec![
            w("first", 30.0, 100.0, 80.0, 112.0),
            w("second", 30.0, 122.0, 80.0, 134.0),
            w("third", 30.0, 144.0, 80.0, 156.0),
        ];
        let lines = assemble(words);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text(), "first");
        assert_eq!(lines[2].text(), "third");
    }

    #[test]
    fn an_oversized_font_box_does_not_swallow_the_line_above() {
        // Page 66 of the Turkish book: ClearScan gives the chapter heading
        // glyphs 42pt-tall boxes for 14pt text, which overlap the chapter
        // number above them. Normalising by font size keeps them apart.
        let mut number = w("8", 245.0, 166.5, 252.8, 208.8);
        number.font_size = 13.9;
        let mut c = w("C", 230.2, 211.1, 241.2, 227.3);
        c.font_size = 15.6;
        let mut ar = w("ar", 240.7, 190.8, 256.1, 233.0);
        ar.font_size = 13.9;
        let mut la = w("la", 255.4, 190.8, 268.8, 233.0);
        la.font_size = 13.9;

        let lines = assemble(vec![number, c, ar, la]);
        assert_eq!(
            lines.len(),
            2,
            "got {:?}",
            lines.iter().map(|l| l.text()).collect::<Vec<_>>()
        );
        assert_eq!(lines[0].text(), "8");
        assert_eq!(lines[1].text(), "C ar la");
    }

    #[test]
    fn leading_is_the_typical_line_pitch() {
        let words = vec![
            w("a", 30.0, 100.0, 80.0, 112.0),
            w("b", 30.0, 122.0, 80.0, 134.0),
            w("c", 30.0, 144.0, 80.0, 156.0),
        ];
        let lines = assemble(words);
        assert!((median_leading(&lines) - 22.0).abs() < 0.01);
    }
}
