//! Line-based three-way merge (diff3) of base, ours and theirs.
//!
//! "Ours" is the pull request's head, "theirs" the base branch, "base" the
//! merge base. Changes that touch or overlap in the base are a conflict
//! unless both sides made the same change (as git does).

use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffOp, capture_diff_slices};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Chunk {
    Clean(Vec<String>),
    Conflict {
        base: Vec<String>,
        ours: Vec<String>,
        theirs: Vec<String>,
    },
}

/// How a conflict chunk is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Choice {
    /// Keep the conflict markers.
    #[default]
    Unresolved,
    Ours,
    Theirs,
    /// Ours, then theirs.
    Both,
    Base,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merge3 {
    pub chunks: Vec<Chunk>,
}

/// Lines with their endings, so joining them gives the text back.
pub fn lines(s: &str) -> Vec<String> {
    s.split_inclusive('\n').map(String::from).collect()
}

/// A change of one side: base lines `[start, end)` become `with`.
#[derive(Clone, Debug)]
struct Change {
    start: usize,
    end: usize,
    with: Vec<String>,
}

fn changes(base: &[String], side: &[String]) -> Vec<Change> {
    let mut out: Vec<Change> = Vec::new();
    for op in capture_diff_slices(Algorithm::Myers, base, side) {
        let c = match op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete {
                old_index, old_len, ..
            } => Change {
                start: old_index,
                end: old_index + old_len,
                with: Vec::new(),
            },
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => Change {
                start: old_index,
                end: old_index,
                with: side[new_index..new_index + new_len].to_vec(),
            },
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => Change {
                start: old_index,
                end: old_index + old_len,
                with: side[new_index..new_index + new_len].to_vec(),
            },
        };
        // Coalesce changes that touch (a delete right after an insert).
        if let Some(last) = out.last_mut()
            && last.end == c.start
        {
            last.end = c.end;
            last.with.extend(c.with);
            continue;
        }
        out.push(c);
    }
    out
}

/// Base `[start, end)` with this side's changes inside it applied.
fn apply(base: &[String], start: usize, end: usize, side: &[&Change]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = start;
    for c in side {
        out.extend_from_slice(&base[i..c.start]);
        out.extend(c.with.iter().cloned());
        i = c.end;
    }
    out.extend_from_slice(&base[i..end]);
    out
}

impl Merge3 {
    pub fn new(base: &str, ours: &str, theirs: &str) -> Self {
        let b = lines(base);
        let o = lines(ours);
        let t = lines(theirs);
        let co = changes(&b, &o);
        let ct = changes(&b, &t);
        let mut chunks: Vec<Chunk> = Vec::new();
        let push_clean = |chunks: &mut Vec<Chunk>, ls: Vec<String>| {
            if ls.is_empty() {
                return;
            }
            if let Some(Chunk::Clean(prev)) = chunks.last_mut() {
                prev.extend(ls);
            } else {
                chunks.push(Chunk::Clean(ls));
            }
        };
        let (mut i, mut j) = (0, 0);
        let mut pos = 0;
        while i < co.len() || j < ct.len() {
            // Start a region at the earliest change.
            let take_ours = match (co.get(i), ct.get(j)) {
                (Some(a), Some(b)) => a.start <= b.start,
                (Some(_), None) => true,
                _ => false,
            };
            let first = if take_ours { &co[i] } else { &ct[j] };
            let (rs, mut re) = (first.start, first.end);
            let (oi, tj) = (i, j);
            if take_ours {
                i += 1;
            } else {
                j += 1;
            }
            // Pull in every change of either side that overlaps or touches.
            loop {
                if let Some(a) = co.get(i)
                    && a.start <= re
                {
                    re = re.max(a.end);
                    i += 1;
                    continue;
                }
                if let Some(b) = ct.get(j)
                    && b.start <= re
                {
                    re = re.max(b.end);
                    j += 1;
                    continue;
                }
                break;
            }
            push_clean(&mut chunks, b[pos..rs].to_vec());
            let ours_c: Vec<&Change> = co[oi..i].iter().collect();
            let theirs_c: Vec<&Change> = ct[tj..j].iter().collect();
            let ours_v = apply(&b, rs, re, &ours_c);
            let theirs_v = apply(&b, rs, re, &theirs_c);
            if theirs_c.is_empty() {
                push_clean(&mut chunks, ours_v);
            } else if ours_c.is_empty() || ours_v == theirs_v {
                push_clean(&mut chunks, theirs_v);
            } else {
                chunks.push(Chunk::Conflict {
                    base: b[rs..re].to_vec(),
                    ours: ours_v,
                    theirs: theirs_v,
                });
            }
            pos = re;
        }
        push_clean(&mut chunks, b[pos..].to_vec());
        Self { chunks }
    }

    pub fn conflicts(&self) -> usize {
        self.chunks
            .iter()
            .filter(|c| matches!(c, Chunk::Conflict { .. }))
            .count()
    }

    pub fn has_conflicts(&self) -> bool {
        self.conflicts() > 0
    }

    /// The text with conflict `k` resolved by `choices[k]` (missing ones keep markers).
    pub fn render(&self, choices: &[Choice]) -> String {
        let mut out = String::new();
        let mut k = 0;
        for c in &self.chunks {
            match c {
                Chunk::Clean(ls) => ls.iter().for_each(|l| out.push_str(l)),
                Chunk::Conflict { base, ours, theirs } => {
                    let choice = choices.get(k).copied().unwrap_or_default();
                    k += 1;
                    match choice {
                        Choice::Ours => put(&mut out, ours),
                        Choice::Theirs => put(&mut out, theirs),
                        Choice::Base => put(&mut out, base),
                        Choice::Both => {
                            put(&mut out, ours);
                            put(&mut out, theirs);
                        }
                        Choice::Unresolved => {
                            out.push_str("<<<<<<< ours\n");
                            put(&mut out, ours);
                            out.push_str("||||||| base\n");
                            put(&mut out, base);
                            out.push_str("=======\n");
                            put(&mut out, theirs);
                            out.push_str(">>>>>>> theirs\n");
                        }
                    }
                }
            }
        }
        out
    }
}

/// Appends lines, ending the last one so a marker can follow.
fn put(out: &mut String, ls: &[String]) {
    for l in ls {
        out.push_str(l);
    }
    if ls.last().is_some_and(|l| !l.ends_with('\n')) {
        out.push('\n');
    }
}

/// Whether text still holds conflict markers.
pub fn has_markers(text: &str) -> bool {
    let mut open = false;
    for l in text.lines() {
        if l.starts_with("<<<<<<< ") {
            open = true;
        } else if open && l.starts_with(">>>>>>> ") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_overlapping_changes_merge_cleanly() {
        let base = "a\nb\nc\nd\ne\n";
        let ours = "A\nb\nc\nd\ne\n";
        let theirs = "a\nb\nc\nd\nE\n";
        let m = Merge3::new(base, ours, theirs);
        assert!(!m.has_conflicts());
        assert_eq!(m.render(&[]), "A\nb\nc\nd\nE\n");
    }

    #[test]
    fn same_change_on_both_sides_is_clean() {
        let m = Merge3::new("a\nb\n", "a\nB\n", "a\nB\n");
        assert!(!m.has_conflicts());
        assert_eq!(m.render(&[]), "a\nB\n");
    }

    #[test]
    fn overlapping_changes_conflict_and_resolve() {
        let base = "one\ntwo\nthree\n";
        let ours = "one\nTWO-ours\nthree\n";
        let theirs = "one\nTWO-theirs\nthree\nfour\n";
        let m = Merge3::new(base, ours, theirs);
        assert_eq!(m.conflicts(), 1);
        assert_eq!(m.render(&[Choice::Ours]), "one\nTWO-ours\nthree\nfour\n");
        assert_eq!(
            m.render(&[Choice::Theirs]),
            "one\nTWO-theirs\nthree\nfour\n"
        );
        assert_eq!(
            m.render(&[Choice::Both]),
            "one\nTWO-ours\nTWO-theirs\nthree\nfour\n"
        );
        assert_eq!(m.render(&[Choice::Base]), "one\ntwo\nthree\nfour\n");
        let marked = m.render(&[]);
        assert!(has_markers(&marked));
        assert!(marked.contains(
            "<<<<<<< ours\nTWO-ours\n||||||| base\ntwo\n=======\nTWO-theirs\n>>>>>>> theirs\n"
        ));
        assert!(!has_markers(&m.render(&[Choice::Ours])));
    }

    #[test]
    fn insertions_at_same_point_conflict() {
        let m = Merge3::new("a\nz\n", "a\nours\nz\n", "a\ntheirs\nz\n");
        assert_eq!(m.conflicts(), 1);
        assert_eq!(m.render(&[Choice::Both]), "a\nours\ntheirs\nz\n");
    }

    #[test]
    fn two_conflicts_choose_each() {
        let base = "1\n2\n3\n4\n5\n6\n7\n";
        let ours = "1\nO2\n3\n4\n5\nO6\n7\n";
        let theirs = "1\nT2\n3\n4\n5\nT6\n7\n";
        let m = Merge3::new(base, ours, theirs);
        assert_eq!(m.conflicts(), 2);
        assert_eq!(
            m.render(&[Choice::Ours, Choice::Theirs]),
            "1\nO2\n3\n4\n5\nT6\n7\n"
        );
    }

    #[test]
    fn missing_final_newline_and_empty_sides() {
        let m = Merge3::new("", "x", "");
        assert_eq!(m.render(&[]), "x");
        let m = Merge3::new("a", "b", "c");
        assert_eq!(m.render(&[Choice::Theirs]), "c\n");
        let m = Merge3::new("a\nb\n", "a\nb\n", "a\nb\n");
        assert_eq!(m.render(&[]), "a\nb\n");
    }
}
