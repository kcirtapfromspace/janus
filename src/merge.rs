//! Turn raw transcripts + speaker information into a labelled, ordered conversation.

use std::collections::HashMap;

use crate::models::{INTERVIEWER, Segment, SpeechSpan, Turn, Word, YOU};

/// A word with no overlapping diarization span borrows the nearest span's speaker if it's this close.
const NEAREST_SPAN_S: f64 = 1.0;

struct SpanIndex<'a> {
    spans: Vec<&'a SpeechSpan>,
}

impl<'a> SpanIndex<'a> {
    fn new(spans: &'a [SpeechSpan]) -> Self {
        let mut spans: Vec<_> = spans.iter().collect();
        spans.sort_by(|a, b| a.start.total_cmp(&b.start));
        SpanIndex { spans }
    }

    /// Speaker whose span overlaps [start, end] the most, else the nearest one within range.
    fn speaker_at(&self, start: f64, end: f64) -> Option<&'a str> {
        // Spans don't overlap, so only a few around the insertion point can touch the word.
        let i = self.spans.partition_point(|s| s.start <= end);
        let lo = i.saturating_sub(3);
        let hi = (i + 1).min(self.spans.len());
        let (mut best, mut best_overlap) = (None, 0.0);
        let (mut nearest, mut nearest_dist) = (None, NEAREST_SPAN_S);
        for span in &self.spans[lo..hi] {
            let overlap = end.min(span.end) - start.max(span.start);
            if overlap > best_overlap {
                (best, best_overlap) = (Some(span.speaker.as_str()), overlap);
            }
            let dist = (span.start - end).max(start - span.end).max(0.0);
            if dist < nearest_dist {
                (nearest, nearest_dist) = (Some(span.speaker.as_str()), dist);
            }
        }
        best.or(nearest)
    }
}

/// Unlabelled words inherit the previous word's speaker (or the next one's at the start).
fn fill_gaps(labels: Vec<Option<&str>>) -> Vec<String> {
    let mut out = labels;
    for i in 1..out.len() {
        if out[i].is_none() {
            out[i] = out[i - 1];
        }
    }
    for i in (0..out.len().saturating_sub(1)).rev() {
        if out[i].is_none() {
            out[i] = out[i + 1];
        }
    }
    out.into_iter().map(|l| l.unwrap_or("unknown").to_string()).collect()
}

fn segment_from_words(words: Vec<Word>, speaker: String) -> Segment {
    Segment {
        start: words[0].start,
        end: words[words.len() - 1].end,
        text: words.iter().map(|w| w.text.as_str()).collect::<String>().trim().to_string(),
        speaker,
        words,
    }
}

/// Label every word by diarization, splitting a Whisper segment wherever the speaker changes.
pub fn assign_speakers(segments: &[Segment], spans: &[SpeechSpan]) -> Vec<Segment> {
    let index = SpanIndex::new(spans);
    let mut out = vec![];
    for seg in segments {
        if seg.words.is_empty() {
            let label = index.speaker_at(seg.start, seg.end).unwrap_or("unknown");
            out.push(Segment { speaker: label.into(), ..seg.clone() });
            continue;
        }
        let labels = fill_gaps(seg.words.iter().map(|w| index.speaker_at(w.start, w.end)).collect());
        let mut run: Vec<Word> = vec![];
        let mut run_label = labels[0].clone();
        for (word, label) in seg.words.iter().zip(labels) {
            if label != run_label {
                out.push(segment_from_words(std::mem::take(&mut run), run_label));
                run_label = label;
            }
            run.push(word.clone());
        }
        out.push(segment_from_words(run, run_label));
    }
    out
}

pub fn talk_time(segments: &[Segment]) -> HashMap<String, f64> {
    let mut totals = HashMap::new();
    for s in segments {
        *totals.entry(s.speaker.clone()).or_insert(0.0) += s.end - s.start;
    }
    totals
}

/// Guess which diarized speaker is you: candidates usually do most of the talking.
///
/// Everyone else becomes an interviewer. `ic swap` fixes a wrong guess; the analysis double-checks.
pub fn default_role_map(segments: &[Segment]) -> HashMap<String, String> {
    let mut ranked: Vec<_> = talk_time(segments).into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .enumerate()
        .map(|(rank, (label, _))| {
            let role = match rank {
                0 => YOU.to_string(),
                1 => INTERVIEWER.to_string(),
                n => format!("{INTERVIEWER}_{n}"),
            };
            (label, role)
        })
        .collect()
}

pub fn relabel(segments: &[Segment], mapping: &HashMap<String, String>) -> Vec<Segment> {
    segments
        .iter()
        .map(|s| Segment { speaker: mapping.get(&s.speaker).cloned().unwrap_or_else(|| s.speaker.clone()), ..s.clone() })
        .collect()
}

pub fn swap_you_and_interviewer(segments: &[Segment]) -> Vec<Segment> {
    let mapping = HashMap::from([(YOU.to_string(), INTERVIEWER.to_string()), (INTERVIEWER.to_string(), YOU.to_string())]);
    relabel(segments, &mapping)
}

/// Dual-track recordings: each track is one known speaker; interleave them by time.
pub fn merge_tracks(tracks: Vec<(&str, Vec<Segment>)>) -> Vec<Segment> {
    let mut all: Vec<Segment> = tracks
        .into_iter()
        .flat_map(|(speaker, segs)| segs.into_iter().map(move |s| Segment { speaker: speaker.to_string(), ..s }))
        .collect();
    all.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
    all
}

pub fn to_turns(segments: &[Segment]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = vec![];
    for s in segments {
        match turns.last_mut() {
            Some(last) if last.speaker == s.speaker => {
                last.end = last.end.max(s.end);
                last.text.push(' ');
                last.text.push_str(&s.text);
            }
            _ => turns.push(Turn { speaker: s.speaker.clone(), start: s.start, end: s.end, text: s.text.clone() }),
        }
    }
    turns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(items: &[(&str, f64, f64)]) -> Vec<Word> {
        items.iter().map(|&(t, s, e)| Word { text: t.into(), start: s, end: e }).collect()
    }

    fn span(start: f64, end: f64, speaker: &str) -> SpeechSpan {
        SpeechSpan { start, end, speaker: speaker.into() }
    }

    #[test]
    fn assign_speakers_splits_a_whisper_segment_at_speaker_change() {
        let seg = Segment {
            words: words(&[(" How", 0.0, 0.3), (" are", 0.3, 0.5), (" you?", 0.5, 0.9), (" Great,", 1.6, 2.0), (" thanks.", 2.0, 2.5)]),
            ..Segment::new(0.0, 4.0, "How are you? Great, thanks.", "")
        };
        let out = assign_speakers(&[seg], &[span(0.0, 1.0, "A"), span(1.5, 3.0, "B")]);
        let got: Vec<_> = out.iter().map(|s| (s.speaker.as_str(), s.text.as_str())).collect();
        assert_eq!(got, [("A", "How are you?"), ("B", "Great, thanks.")]);
        assert_eq!((out[1].start, out[1].end), (1.6, 2.5));
    }

    #[test]
    fn word_between_spans_takes_nearest_speaker_and_gaps_inherit() {
        let seg = Segment {
            words: words(&[(" one", 0.0, 0.4), (" two", 1.2, 1.4), (" far", 10.0, 10.2)]),
            ..Segment::new(0.0, 6.0, "x", "")
        };
        let out = assign_speakers(&[seg], &[span(0.0, 1.0, "A"), span(3.0, 5.0, "B")]);
        assert_eq!(out.iter().map(|s| s.speaker.as_str()).collect::<Vec<_>>(), ["A"]);
    }

    #[test]
    fn segment_without_words_uses_segment_overlap() {
        let out = assign_speakers(&[Segment::new(2.0, 4.0, "hi", "")], &[span(1.5, 5.0, "B")]);
        assert_eq!(out[0].speaker, "B");
    }

    #[test]
    fn default_role_map_makes_the_biggest_talker_you() {
        let segs = [
            Segment::new(0.0, 5.0, "q", "SPEAKER_00"),
            Segment::new(5.0, 25.0, "long answer", "SPEAKER_01"),
            Segment::new(25.0, 27.0, "panel q", "SPEAKER_02"),
        ];
        let mapping = default_role_map(&segs);
        assert_eq!(mapping["SPEAKER_01"], YOU);
        assert_eq!(mapping["SPEAKER_00"], INTERVIEWER);
        assert_eq!(mapping["SPEAKER_02"], "interviewer_2");
        let speakers: Vec<_> = relabel(&segs, &mapping).into_iter().map(|s| s.speaker).collect();
        assert_eq!(speakers, [INTERVIEWER, YOU, "interviewer_2"]);
    }

    #[test]
    fn swap_you_and_interviewer_swaps_both_ways() {
        let segs = [Segment::new(0.0, 1.0, "a", YOU), Segment::new(1.0, 2.0, "b", INTERVIEWER)];
        let speakers: Vec<_> = swap_you_and_interviewer(&segs).into_iter().map(|s| s.speaker).collect();
        assert_eq!(speakers, [INTERVIEWER, YOU]);
    }

    #[test]
    fn merge_tracks_interleaves_by_time_and_to_turns_groups_runs() {
        let mine = vec![Segment::new(5.0, 8.0, "My answer.", ""), Segment::new(8.5, 10.0, "More detail.", "")];
        let theirs = vec![Segment::new(0.0, 4.0, "A question?", ""), Segment::new(11.0, 12.0, "Thanks.", "")];
        let merged = merge_tracks(vec![(YOU, mine), (INTERVIEWER, theirs)]);
        let texts: Vec<_> = merged.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["A question?", "My answer.", "More detail.", "Thanks."]);
        let turns = to_turns(&merged);
        let got: Vec<_> = turns.iter().map(|t| (t.speaker.as_str(), t.text.as_str())).collect();
        assert_eq!(got, [(INTERVIEWER, "A question?"), (YOU, "My answer. More detail."), (INTERVIEWER, "Thanks.")]);
        assert_eq!((turns[1].start, turns[1].end), (5.0, 10.0));
    }
}
