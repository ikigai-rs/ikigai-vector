//! Nearest, `urn:vector:nearest:index:{n}` (`text=`, `k=`): the `k` nearest passages to a
//! query, by NAME, rank and score; never their text, and never the query's.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, Error, Invocation, ReprType, Representation, Result, Verb,
};
use oxrdf::{Literal, Triple};
use serde_json::json;

use crate::embedding::embed;
use crate::index::{change_thread, digest_mismatch};
use crate::{
    count, face, index_binding, named, normalize, read_index_scope, require, term, turtle, Hit,
    Inner, INDEX_PREFIX, JSON, NEAREST_ID, NEAREST_PREFIX, READ_INDEX, TEXT, TURTLE, XSD_INTEGER,
    XSD_STRING,
};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD_DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";

/// The endpoint at [`NEAREST_TEMPLATE`](crate::NEAREST_TEMPLATE).
pub(crate) struct NearestEndpoint {
    pub(crate) inner: Arc<Inner>,
}

/// What an answer says about the index beside its hits.
struct Answer {
    n: String,
    exact: bool,
    model: String,
    digest: Option<String>,
    hits: Vec<Hit>,
}

impl Answer {
    fn text(&self) -> String {
        let quality = if self.exact { "exact" } else { "approximate" };
        let mut out = match &self.digest {
            Some(digest) => format!(
                "{}, {quality}, model {} digest {digest}\n",
                count(self.hits.len(), "passage", "passages"),
                self.model
            ),
            None => format!(
                "{}, {quality}, model {}: the index holds no vectors\n",
                count(self.hits.len(), "passage", "passages"),
                self.model
            ),
        };
        for (i, hit) in self.hits.iter().enumerate() {
            out.push_str(&format!(
                "  {}. {}  score {}\n",
                i + 1,
                hit.passage,
                hit.score
            ));
        }
        out
    }

    fn json(&self) -> Result<Vec<u8>> {
        let hits: Vec<_> = self
            .hits
            .iter()
            .enumerate()
            .map(|(i, hit)| {
                json!({ "rank": i + 1, "passage": hit.passage, "score": hit.score.as_f64() })
            })
            .collect();
        serde_json::to_vec(&json!({
            "index": self.n,
            "exact": self.exact,
            "model": self.model,
            "digest": self.digest,
            "count": self.hits.len(),
            "hits": hits,
        }))
        .map_err(|e| Error::Endpoint(format!("writing the nearest answer: {e}")))
    }

    fn turtle(&self) -> Result<Vec<u8>> {
        let answer_iri = format!("{NEAREST_PREFIX}{}", self.n);
        let answer = named(&answer_iri)?;
        let mut triples = vec![
            Triple::new(answer.clone(), named(RDF_TYPE)?, term("NearestAnswer")),
            Triple::new(
                answer.clone(),
                term("index"),
                named(&format!("{INDEX_PREFIX}{}", self.n))?,
            ),
            Triple::new(answer.clone(), term("exact"), Literal::from(self.exact)),
            Triple::new(
                answer.clone(),
                term("model"),
                Literal::new_simple_literal(&self.model),
            ),
            Triple::new(
                answer.clone(),
                term("hitCount"),
                Literal::from(self.hits.len() as i64),
            ),
        ];
        if let Some(digest) = &self.digest {
            triples.push(Triple::new(
                answer.clone(),
                term("modelDigest"),
                Literal::new_simple_literal(digest),
            ));
        }
        let node = |rank: usize| named(&format!("{answer_iri}#rank-{rank}"));
        for rank in 1..=self.hits.len() {
            triples.push(Triple::new(answer.clone(), term("hit"), node(rank)?));
        }
        for (i, hit) in self.hits.iter().enumerate() {
            let rank = i + 1;
            let node = node(rank)?;
            triples.push(Triple::new(node.clone(), named(RDF_TYPE)?, term("Hit")));
            triples.push(Triple::new(
                node.clone(),
                term("rank"),
                Literal::from(rank as i64),
            ));
            triples.push(Triple::new(
                node.clone(),
                term("passage"),
                named(&hit.passage)?,
            ));
            triples.push(Triple::new(
                node,
                term("score"),
                Literal::new_typed_literal(hit.score.to_string(), named(XSD_DECIMAL)?),
            ));
        }
        turtle(
            &triples,
            &format!("the nearest answer of index `{}`", self.n),
        )
    }
}

#[async_trait]
impl Endpoint for NearestEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let n = index_binding(inv)?;
        let face = face(inv, &[TEXT, TURTLE, JSON], "a nearest answer")?;
        require(inv, &read_index_scope(n), &format!("reading index `{n}`"))?;
        inv.depends_on(change_thread(n));
        inv.depends_on(format!("{INDEX_PREFIX}{n}"));
        let (model, provider, max_k) = {
            let indexes = self.inner.indexes.lock().expect("index lock");
            let Some(state) = indexes.get(n) else {
                return Err(Error::NotFound(format!(
                    "index `{n}` is not built: add a member with `sink \
                     urn:vector:member:index:{n}:passage:{{p}}`, or build it empty with `sink \
                     {INDEX_PREFIX}{n}`"
                )));
            };
            let d = &state.declaration;
            (d.model.clone(), d.provider.clone(), d.max_k)
        };
        let k_word = inv
            .inline_str("k")
            .map_err(|_| Error::MissingArgument("k".to_string()))?;
        let k = k_word
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|k| (1..=max_k).contains(k))
            .ok_or_else(|| Error::InvalidArgument {
                name: "k".to_string(),
                detail: format!(
                    "k must be a whole number from 1 to {max_k}, the bound the declaration of \
                     index `{n}` sets"
                ),
            })?;
        // The query is data: it is embedded, never quoted back.
        let text = inv
            .inline_str("text")
            .map_err(|_| Error::MissingArgument("text".to_string()))?;
        if text.trim().is_empty() {
            return Err(Error::InvalidArgument {
                name: "text".to_string(),
                detail: "`text` is empty, so there is nothing to embed".to_string(),
            });
        }
        let empty = {
            let indexes = self.inner.indexes.lock().expect("index lock");
            indexes.get(n).is_none_or(|s| s.backend.is_empty())
        };
        let query = if empty {
            None
        } else {
            let embedded = embed(inv, &model, provider.as_deref(), text).await?;
            let vector = normalize(&embedded.vector).map_err(|why| {
                Error::Endpoint(format!("model {model} embedded the query, and {why}"))
            })?;
            Some((embedded.digest, vector))
        };
        let answer = {
            let indexes = self.inner.indexes.lock().expect("index lock");
            let Some(state) = indexes.get(n) else {
                return Err(Error::NotFound(format!("index `{n}` is not built")));
            };
            let hits = match (&query, &state.digest) {
                (Some((got, vector)), Some(held)) => {
                    if got != held {
                        return Err(digest_mismatch(n, &model, held, got));
                    }
                    if vector.len() != state.backend.dimensions() {
                        return Err(Error::Conflict(format!(
                            "model {model} embedded the query in {}, and index `{n}` holds {}",
                            count(vector.len(), "dimension", "dimensions"),
                            count(state.backend.dimensions(), "dimension", "dimensions"),
                        )));
                    }
                    state.backend.nearest(vector, k as usize)
                }
                _ => Vec::new(),
            };
            Answer {
                n: n.to_string(),
                exact: state.backend.exact(),
                model,
                digest: state.digest.clone(),
                hits,
            }
        };
        let bytes = match face {
            TEXT => answer.text().into_bytes(),
            TURTLE => answer.turtle()?,
            _ => answer.json()?,
        };
        Ok(Representation::new(ReprType::new(face), bytes).cacheable())
    }

    fn describe(&self) -> Description {
        Description::new(NEAREST_ID)
            .title("Nearest passages")
            .summary(
                "The k nearest passages in index {n} to the query text, embedded with the \
                 index's model: passage names with rank and score, never passage text, so reading \
                 a passage stays under the caller's own capability. Scores are cosine \
                 similarities rounded to 6 decimal places before ranking, ties by passage name; \
                 every answer says whether it is exact. k runs from 1 to the bound the \
                 declaration sets. The text is never quoted back. text/plain by default, \
                 as=text/turtle or as=application/json. Hangs on the index's thread: a composite \
                 takes its names as a value rather than joining it.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(READ_INDEX)
            .input(
                ArgSpec::new("n")
                    .summary("the index's name")
                    .class(XSD_STRING)
                    .binding(),
            )
            .input(
                ArgSpec::new("text")
                    .summary("the query text; data, never quoted back")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("k")
                    .summary(
                        "how many passages, from 1 to the declaration's bound (50 unless it says)",
                    )
                    .class(XSD_INTEGER),
            )
            .output(TEXT)
            .output(TURTLE)
            .output(JSON)
    }
}
