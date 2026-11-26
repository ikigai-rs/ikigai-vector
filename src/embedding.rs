//! The embedding, `urn:vector:embedding:model:{m}:hash:{h}`: a composition over
//! `urn:llm:embed`, pure and cacheable on the text and the model.
//!
//! `{h}` is the SHA-256 of the text, so the name alone cannot be computed: the caller supplies
//! the text as `text=`, and only the member endpoint (which holds it) and nearest (which holds
//! the query) do. A direct `source` with no text is `NotFound`, naming the member endpoint as
//! the way in. The text is data: it goes to `urn:llm:embed` only, never into an error or a
//! trace note.

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, Error, Invocation, ReprType, Representation, Request,
    Result, Verb,
};
use serde_json::{json, Value};

use crate::declaration::check_model;
use crate::{
    embedding_name, iri, sha256_hex, EMBEDDING_ID, JSON, LLM_EMBED, MEMBER_TEMPLATE, XSD_STRING,
};

/// What an embedding answered: the model as installed, its digest, and the vector as the model
/// produced it (not yet normalized).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Embedded {
    pub(crate) digest: String,
    pub(crate) vector: Vec<f32>,
}

/// Embed `text` with `model` (and `provider`, when the declaration names one) through the
/// embedding endpoint, so the kernel caches it under the embedding's name and the request's
/// digest, never the text.
pub(crate) async fn embed(
    inv: &Invocation<'_>,
    model: &str,
    provider: Option<&str>,
    text: &str,
) -> Result<Embedded> {
    let mut request = Request::new(Verb::Source, iri(&embedding_name(model, text))?)
        .with_arg("text", ArgRef::Inline(text.as_bytes().to_vec()));
    if let Some(provider) = provider {
        request = request.with_arg("provider", ArgRef::Inline(provider.as_bytes().to_vec()));
    }
    let repr = inv.issue(request).await?;
    let answer: Value = serde_json::from_slice(&repr.bytes)
        .map_err(|e| Error::Endpoint(format!("the embedding answer is not JSON: {e}")))?;
    parse(&answer, "the embedding answer")
}

/// Read `{vector, digest, dimensions}` out of an embedding answer, checking the vector's length
/// against `dimensions`.
fn parse(answer: &Value, what: &str) -> Result<Embedded> {
    let bad = |why: &str| Error::Endpoint(format!("{what} {why}"));
    let digest = answer
        .get("digest")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .ok_or_else(|| bad("names no model digest"))?;
    let values = answer
        .get("vector")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("carries no vector"))?;
    let vector = values
        .iter()
        .map(|v| v.as_f64().map(|f| f as f32))
        .collect::<Option<Vec<f32>>>()
        .ok_or_else(|| bad("has a vector value that is not a number"))?;
    if vector.iter().any(|v| !v.is_finite()) {
        return Err(bad("has a vector value that is not a finite f32 number"));
    }
    if let Some(dimensions) = answer.get("dimensions") {
        let d = dimensions
            .as_u64()
            .ok_or_else(|| bad("has a dimensions value that is not an unsigned integer"))?;
        if d != vector.len() as u64 {
            return Err(bad(&format!(
                "says {d} dimensions and carries {} values",
                vector.len()
            )));
        }
    }
    Ok(Embedded {
        digest: digest.to_string(),
        vector,
    })
}

/// The endpoint at [`EMBEDDING_TEMPLATE`](crate::EMBEDDING_TEMPLATE).
pub(crate) struct EmbeddingEndpoint;

#[async_trait]
impl Endpoint for EmbeddingEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let m = inv
            .bindings
            .get("m")
            .ok_or_else(|| Error::MissingArgument("m".to_string()))?;
        check_model(m).map_err(|detail| Error::InvalidArgument {
            name: "m".to_string(),
            detail,
        })?;
        let h = inv
            .bindings
            .get("h")
            .ok_or_else(|| Error::MissingArgument("h".to_string()))?;
        if h.len() != 64 || !h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(Error::InvalidArgument {
                name: "h".to_string(),
                detail: format!(
                    "`{h}` is not a SHA-256 in lowercase hex: 64 characters of 0-9 and a-f"
                ),
            });
        }
        let Ok(text) = inv.inline_str("text") else {
            return Err(Error::NotFound(format!(
                "the embedding of hash {h} by model {m} is not cached here, and a hash alone \
                 cannot be embedded: an embedding is made when its passage is added to an index, \
                 `sink {MEMBER_TEMPLATE}`, which holds the text"
            )));
        };
        if sha256_hex(text.as_bytes()) != *h {
            return Err(Error::InvalidArgument {
                name: "text".to_string(),
                detail: format!("the SHA-256 of `text` is not the hash {h} this name carries"),
            });
        }
        let mut request = Request::new(Verb::Source, iri(LLM_EMBED)?)
            .with_arg("model", ArgRef::Inline(m.as_bytes().to_vec()))
            .with_arg("text", ArgRef::Inline(text.as_bytes().to_vec()))
            .with_arg("as", ArgRef::Inline(JSON.as_bytes().to_vec()));
        if let Ok(provider) = inv.inline_str("provider") {
            request = request.with_arg("provider", ArgRef::Inline(provider.as_bytes().to_vec()));
        }
        let repr = match inv.issue(request).await {
            Ok(repr) => repr,
            Err(Error::Unresolved(_)) => {
                inv.depends_on(LLM_EMBED);
                return Err(Error::NotFound(format!(
                    "an embedding is made by {LLM_EMBED}, and nothing serves it in this kernel; a \
                     host binds ikigai-llm beside this module"
                )));
            }
            Err(e) => return Err(e),
        };
        let answer: Value = serde_json::from_slice(&repr.bytes)
            .map_err(|e| Error::Endpoint(format!("{LLM_EMBED} did not answer JSON: {e}")))?;
        let embedded = parse(&answer, LLM_EMBED)?;
        let model = answer.get("model").and_then(Value::as_str).unwrap_or(m);
        let body = json!({
            "model": model,
            "digest": embedded.digest,
            "dimensions": embedded.vector.len(),
            "vector": embedded.vector,
        });
        let bytes = serde_json::to_vec(&body)
            .map_err(|e| Error::Endpoint(format!("writing the embedding: {e}")))?;
        // Cacheable on the text and the model; it carries urn:llm:embed's threads (its config
        // and the provider's installed models), so a re-pulled model recomputes it.
        Ok(Representation::new(ReprType::new(JSON), bytes).cacheable())
    }

    fn describe(&self) -> Description {
        Description::new(EMBEDDING_ID)
            .title("Passage embedding")
            .summary(
                "The embedding of a text by model {m}, where {h} is the SHA-256 of the text in \
                 lowercase hex, made by urn:llm:embed. The text is given as text= and checked \
                 against {h}; it goes to the model only and is never quoted back. Without it the \
                 name is not-found, since a hash cannot be embedded: adding a passage to an index \
                 (sink urn:vector:member:index:{n}:passage:{p}) is the way in. application/json: \
                 model, digest, dimensions and vector. Cacheable on the text and the model.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("m")
                    .summary("the embedding model, as ikigai-llm installs it")
                    .class(XSD_STRING)
                    .binding(),
            )
            .input(
                ArgSpec::new("h")
                    .summary("the SHA-256 of the text, 64 lowercase hex characters")
                    .class(XSD_STRING)
                    .binding(),
            )
            .input(
                ArgSpec::new("text")
                    .summary(
                        "the text to embed, whose SHA-256 is {h}; without it the name is \
                         not-found",
                    )
                    .class(XSD_STRING)
                    .optional(),
            )
            .input(
                ArgSpec::new("provider")
                    .summary("the embedding provider; without it, the host's default")
                    .class(XSD_STRING)
                    .optional(),
            )
            .output(JSON)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_present_dimensions_field_must_be_an_unsigned_integer() {
        for dimensions in [json!("2"), json!(-1), json!(2.5), Value::Null] {
            let answer = json!({
                "digest": "sha256:test",
                "dimensions": dimensions,
                "vector": [1.0, 2.0],
            });
            assert!(
                matches!(
                    parse(&answer, LLM_EMBED),
                    Err(Error::Endpoint(ref why))
                        if why == "urn:llm:embed has a dimensions value that is not an unsigned integer"
                ),
                "{dimensions}"
            );
        }
    }

    #[test]
    fn a_number_outside_f32_range_is_not_a_valid_embedding_component() {
        for component in [1e39, -1e39] {
            let answer = json!({
                "digest": "sha256:test",
                "dimensions": 2,
                "vector": [1.0, component],
            });
            assert!(
                matches!(
                    parse(&answer, LLM_EMBED),
                    Err(Error::Endpoint(ref why))
                        if why == "urn:llm:embed has a vector value that is not a finite f32 number"
                ),
                "{component}"
            );
        }
    }
}
