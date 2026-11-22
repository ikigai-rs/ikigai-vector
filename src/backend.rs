//! Backends behind one contract: what a backend holds, the score it answers, and the tie rule
//! every backend ranks by (design, section 2).
//!
//! A backend holds **passage names and vectors, nothing else**: no method takes passage or
//! query text, so text cannot enter one (guarantee 5). The module, above the backend, embeds
//! text, checks the model digest and L2-normalizes every vector before a backend sees it.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// A score: a cosine similarity of two L2-normalized vectors, **rounded to 6 decimal places
/// before ranking** (guarantee 1), held as an integer count of millionths so that ranking and
/// printing never see the float again.
///
/// Rounding absorbs the last-bit differences a different summation order produces, so the same
/// index state, query and `k` give the same list on every machine.
///
/// ```
/// use ikigai_vector::Score;
///
/// assert_eq!(Score::from_cosine(0.8123454).to_string(), "0.812345");
/// assert_eq!(Score::from_cosine(0.8123456).to_string(), "0.812346");
/// assert_eq!(Score::from_cosine(1.0).to_string(), "1.000000");
/// assert_eq!(Score::from_cosine(-0.25).to_string(), "-0.250000");
/// // A value that rounds to zero is zero, never "-0.000000".
/// assert_eq!(Score::from_cosine(-0.0000004).to_string(), "0.000000");
/// assert_eq!(Score::from_cosine(0.5).millionths(), 500_000);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Score(i64);

impl Score {
    /// The score of a cosine similarity, rounded half away from zero to 6 decimal places.
    pub fn from_cosine(cosine: f64) -> Score {
        Score((cosine * 1_000_000.0).round() as i64)
    }

    /// The score in millionths: `0.812345` is `812345`.
    pub fn millionths(self) -> i64 {
        self.0
    }

    /// The score as a float, for a face that carries numbers (`application/json`).
    pub fn as_f64(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }
}

impl fmt::Display for Score {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let magnitude = self.0.unsigned_abs();
        write!(
            f,
            "{sign}{}.{:06}",
            magnitude / 1_000_000,
            magnitude % 1_000_000
        )
    }
}

/// One passage in a nearest answer: its name and its score. The rank is its position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The passage's name, an IRI.
    pub passage: String,
    /// Its rounded score.
    pub score: Score,
}

/// The tie rule every backend answers by (guarantee 1): rounded score descending, then passage
/// name ascending, comparing bytes; the first `k`.
///
/// A backend that cannot rank exactly this way itself (an external service) asks for a few
/// more than `k` and ranks what it got with this function, so a tie that straddles the
/// boundary is decided by name, never by the service's internal order.
///
/// ```
/// use ikigai_vector::{rank, Hit, Score};
///
/// let s = Score::from_cosine;
/// let hits = rank(
///     vec![
///         Hit { passage: "urn:p:b".into(), score: s(0.5) },
///         Hit { passage: "urn:p:c".into(), score: s(0.9) },
///         Hit { passage: "urn:p:a".into(), score: s(0.5) },
///     ],
///     2,
/// );
/// let names: Vec<&str> = hits.iter().map(|h| h.passage.as_str()).collect();
/// assert_eq!(names, ["urn:p:c", "urn:p:a"]);
/// ```
pub fn rank(mut candidates: Vec<Hit>, k: usize) -> Vec<Hit> {
    let order = |a: &Hit, b: &Hit| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.passage.as_bytes().cmp(b.passage.as_bytes()))
    };
    if k < candidates.len() {
        // Names are unique within an index, so the order is total and the selection is the
        // same whatever the input order.
        candidates.select_nth_unstable_by(k, order);
        candidates.truncate(k);
    }
    candidates.sort_unstable_by(order);
    candidates
}

/// L2-normalize a vector: every value finite, and not the zero vector, which has no direction.
/// The norm is summed in `f64`.
///
/// ```
/// use ikigai_vector::normalize;
///
/// assert_eq!(normalize(&[3.0, 4.0]).unwrap(), vec![0.6, 0.8]);
/// assert!(normalize(&[0.0, 0.0]).is_err());
/// assert!(normalize(&[f32::NAN, 1.0]).is_err());
/// ```
pub fn normalize(vector: &[f32]) -> Result<Vec<f32>, String> {
    if vector.iter().any(|v| !v.is_finite()) {
        return Err("the vector holds a value that is not a finite number".to_string());
    }
    let norm = vector
        .iter()
        .map(|&v| f64::from(v) * f64::from(v))
        .sum::<f64>()
        .sqrt();
    if norm == 0.0 {
        return Err("the vector is all zeros, so it has no direction to compare".to_string());
    }
    Ok(vector
        .iter()
        .map(|&v| (f64::from(v) / norm) as f32)
        .collect())
}

/// A backend: the vectors of one index, held under passage names.
///
/// Every backend meets the five guarantees of the design (section 2), and
/// `tests/backend.rs` checks them through the kernel for each backend this crate offers:
///
/// 1. **Deterministic order, ties included**: [`nearest`](Backend::nearest) answers by
///    [`rank`]'s rule over [`Score`]s.
/// 2. **Delete is immediate and total**: once [`remove`](Backend::remove) returns, no
///    `nearest` answers that name.
/// 3. **Rebuild is equivalent**: a backend filled from the member list answers what the
///    incrementally maintained one answers (identically, for an exact backend).
/// 4. **Exactness is declared**: [`exact`](Backend::exact), carried into every answer.
/// 5. **No text kept**: names and vectors only; no method takes text.
///
/// Vectors arrive L2-normalized and of the declared [`dimensions`](Backend::dimensions); the
/// module checks both before calling.
pub trait Backend: Send + Sync {
    /// The backend's kind, as a declaration names it (`flat`).
    fn kind(&self) -> &str;
    /// Whether every answer is exact (guarantee 4).
    fn exact(&self) -> bool;
    /// How many values each vector holds.
    fn dimensions(&self) -> usize;
    /// Hold `vector` under `passage`, replacing what it held there.
    fn insert(&mut self, passage: &str, vector: &[f32]);
    /// Drop `passage`; whether it was held.
    fn remove(&mut self, passage: &str) -> bool;
    /// Whether `passage` is held.
    fn contains(&self, passage: &str) -> bool;
    /// How many passages are held.
    fn len(&self) -> usize;
    /// Whether no passage is held.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Every passage held, ascending by bytes.
    fn passages(&self) -> Vec<String>;
    /// Everything the backend holds, ascending by passage name: names and vectors, nothing
    /// else (guarantee 5 is checked against this).
    fn entries(&self) -> Vec<(String, Vec<f32>)>;
    /// The `k` nearest passages to an L2-normalized `query`, by [`rank`]'s rule.
    fn nearest(&self, query: &[f32], k: usize) -> Vec<Hit>;
}

/// Makes an empty backend of the given dimensions. A host registers one per backend kind it
/// offers ([`Vectors::with_backend`](crate::Vectors::with_backend)).
pub type BackendFactory = Arc<dyn Fn(usize) -> Box<dyn Backend> + Send + Sync>;

/// The kind name of [`Flat`].
pub const FLAT: &str = "flat";

/// `flat`: an exact in-process scan with no crate (design, section 2). A dot product over one
/// contiguous `Vec<f32>` (summed in `f64`), then [`rank`]. Delete is a swap-remove, and
/// rebuild is a reload. It compiles for `wasm32-unknown-unknown`.
///
/// ```
/// use ikigai_vector::{Backend, Flat};
///
/// let mut flat = Flat::new(2);
/// flat.insert("urn:p:a", &[1.0, 0.0]);
/// flat.insert("urn:p:b", &[0.6, 0.8]);
/// let hits = flat.nearest(&[1.0, 0.0], 2);
/// assert_eq!(hits[0].passage, "urn:p:a");
/// assert_eq!(hits[0].score.to_string(), "1.000000");
/// assert_eq!(hits[1].score.to_string(), "0.600000");
/// assert!(flat.remove("urn:p:a"));
/// assert_eq!(flat.nearest(&[1.0, 0.0], 2).len(), 1);
/// ```
#[derive(Clone, Debug)]
pub struct Flat {
    dimensions: usize,
    names: Vec<String>,
    values: Vec<f32>,
    at: HashMap<String, usize>,
}

impl Flat {
    /// An empty flat backend of `dimensions`.
    pub fn new(dimensions: usize) -> Flat {
        Flat {
            dimensions,
            names: Vec::new(),
            values: Vec::new(),
            at: HashMap::new(),
        }
    }

    /// The factory a host registers under [`FLAT`].
    pub fn factory() -> BackendFactory {
        Arc::new(|dimensions| Box::new(Flat::new(dimensions)))
    }

    fn row(&self, i: usize) -> &[f32] {
        &self.values[i * self.dimensions..(i + 1) * self.dimensions]
    }
}

impl Backend for Flat {
    fn kind(&self) -> &str {
        FLAT
    }

    fn exact(&self) -> bool {
        true
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn insert(&mut self, passage: &str, vector: &[f32]) {
        debug_assert_eq!(vector.len(), self.dimensions);
        match self.at.get(passage) {
            Some(&i) => {
                let d = self.dimensions;
                self.values[i * d..(i + 1) * d].copy_from_slice(vector);
            }
            None => {
                self.at.insert(passage.to_string(), self.names.len());
                self.names.push(passage.to_string());
                self.values.extend_from_slice(vector);
            }
        }
    }

    fn remove(&mut self, passage: &str) -> bool {
        let Some(i) = self.at.remove(passage) else {
            return false;
        };
        let last = self.names.len() - 1;
        let d = self.dimensions;
        if i != last {
            // Move the last row into the hole.
            self.values.copy_within(last * d..(last + 1) * d, i * d);
            self.names.swap(i, last);
            self.at.insert(self.names[i].clone(), i);
        }
        self.names.pop();
        self.values.truncate(last * d);
        true
    }

    fn contains(&self, passage: &str) -> bool {
        self.at.contains_key(passage)
    }

    fn len(&self) -> usize {
        self.names.len()
    }

    fn passages(&self) -> Vec<String> {
        let mut names = self.names.clone();
        names.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        names
    }

    fn entries(&self) -> Vec<(String, Vec<f32>)> {
        self.passages()
            .into_iter()
            .map(|name| {
                let row = self.row(self.at[&name]).to_vec();
                (name, row)
            })
            .collect()
    }

    fn nearest(&self, query: &[f32], k: usize) -> Vec<Hit> {
        let candidates = (0..self.names.len())
            .map(|i| {
                let dot: f64 = self
                    .row(i)
                    .iter()
                    .zip(query)
                    .map(|(&a, &b)| f64::from(a) * f64::from(b))
                    .sum();
                Hit {
                    passage: self.names[i].clone(),
                    score: Score::from_cosine(dot),
                }
            })
            .collect();
        rank(candidates, k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_swap_remove_keeps_every_other_row() {
        let mut flat = Flat::new(2);
        flat.insert("urn:p:a", &[1.0, 0.0]);
        flat.insert("urn:p:b", &[0.0, 1.0]);
        flat.insert("urn:p:c", &[0.6, 0.8]);
        assert!(flat.remove("urn:p:a"));
        assert!(!flat.remove("urn:p:a"));
        assert_eq!(
            flat.entries(),
            vec![
                ("urn:p:b".to_string(), vec![0.0, 1.0]),
                ("urn:p:c".to_string(), vec![0.6, 0.8]),
            ]
        );
    }

    #[test]
    fn an_insert_under_a_held_name_replaces_it() {
        let mut flat = Flat::new(2);
        flat.insert("urn:p:a", &[1.0, 0.0]);
        flat.insert("urn:p:a", &[0.0, 1.0]);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat.entries()[0].1, vec![0.0, 1.0]);
    }

    #[test]
    fn rank_is_the_same_whatever_the_input_order() {
        let s = Score::from_cosine;
        let hits = || {
            vec![
                Hit {
                    passage: "urn:p:d".into(),
                    score: s(0.1),
                },
                Hit {
                    passage: "urn:p:b".into(),
                    score: s(0.5),
                },
                Hit {
                    passage: "urn:p:a".into(),
                    score: s(0.5),
                },
                Hit {
                    passage: "urn:p:c".into(),
                    score: s(0.5),
                },
            ]
        };
        let mut reversed = hits();
        reversed.reverse();
        assert_eq!(rank(hits(), 3), rank(reversed, 3));
        let names: Vec<String> = rank(hits(), 3).into_iter().map(|h| h.passage).collect();
        assert_eq!(names, ["urn:p:a", "urn:p:b", "urn:p:c"]);
        assert_eq!(rank(hits(), 10).len(), 4);
    }
}
