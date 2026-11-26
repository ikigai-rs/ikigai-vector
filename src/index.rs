//! The index (`urn:vector:index:name:{n}`), its members
//! (`urn:vector:member:index:{n}:passage:{p}`) and its change thread
//! (`urn:vector:change:index:{n}`).
//!
//! The index is an ATOM only because a backend is stateful; it is a cache, rebuilt from the
//! declaration, the member list and the embeddings. Its `source` describes it and never carries
//! a vector.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgSpec, Description, Endpoint, Error, Invocation, ReprType, Representation,
    Result, Verb,
};
use oxrdf::{Literal, Triple};

use crate::declaration::{self, Declaration};
use crate::embedding::embed;
use crate::{
    announce, count, face, index_binding, iri, named, normalize, plain, read_index_scope, require,
    sha256_hex, term, turtle, write_index_scope, IndexState, Inner, CHANGE_ID, CHANGE_PREFIX,
    DECLARATION_PREFIX, FS_READ, INDEX_ID, INDEX_PREFIX, MEMBER_ID, READ_INDEX, TEXT, TURTLE,
    WRITE_INDEX, XSD_ANY_URI, XSD_STRING,
};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The thread every member change and rebuild of index `n` cuts.
pub(crate) fn change_thread(n: &str) -> String {
    format!("{CHANGE_PREFIX}{n}")
}

fn rebuild_hint(n: &str) -> String {
    format!("rebuild it with `sink {INDEX_PREFIX}{n}`")
}

/// The refusal when vectors from two model digests would meet.
pub(crate) fn digest_mismatch(n: &str, model: &str, held: &str, got: &str) -> Error {
    Error::Conflict(format!(
        "index `{n}` holds vectors from model {model} digest {held}, and the embedding came from \
         digest {got}; vectors from two digests are never compared, so {}",
        rebuild_hint(n)
    ))
}

fn index_n_binding() -> ArgSpec {
    ArgSpec::new("n")
        .summary("the index's name, declared at vector/index/{n}.ttl in the config home")
        .class(XSD_STRING)
        .binding()
}

/// The digest of a member list: the SHA-256 of each name followed by a newline, in byte order.
fn member_list_digest(passages: &[String]) -> String {
    let mut joined = String::new();
    for p in passages {
        joined.push_str(p);
        joined.push('\n');
    }
    format!("sha256:{}", sha256_hex(joined.as_bytes()))
}

/// Read a passage's text through the kernel under the caller's capability, and embed it with the
/// declaration's model. Answers the digest and the L2-normalized vector, its length checked
/// against the declaration.
async fn embed_passage(
    inv: &Invocation<'_>,
    declaration: &Declaration,
    passage: &str,
) -> Result<(String, Vec<f32>)> {
    let n = &declaration.index;
    let repr = match inv.source(&iri(passage)?).await {
        Ok(repr) => repr,
        Err(Error::NotFound(why)) => {
            return Err(Error::NotFound(format!(
                "the passage {passage} is not there to add to index `{n}`: {why}"
            )))
        }
        Err(Error::Unresolved(_)) => {
            return Err(Error::NotFound(format!(
                "nothing serves the passage {passage} in this kernel, so index `{n}` cannot add it"
            )))
        }
        Err(e) => return Err(e),
    };
    let text = std::str::from_utf8(&repr.bytes).map_err(|_| Error::InvalidArgument {
        name: "p".to_string(),
        detail: format!("the passage {passage} is not UTF-8 text, so it cannot be embedded"),
    })?;
    if text.trim().is_empty() {
        return Err(Error::InvalidArgument {
            name: "p".to_string(),
            detail: format!("the passage {passage} has no text to embed"),
        });
    }
    let embedded = embed(
        inv,
        &declaration.model,
        declaration.provider.as_deref(),
        text,
    )
    .await?;
    if embedded.vector.len() != declaration.dimensions {
        return Err(Error::Conflict(format!(
            "model {} answered {}, and the declaration of index `{n}` says {}; fix the \
             declaration or the model",
            declaration.model,
            count(embedded.vector.len(), "dimension", "dimensions"),
            count(declaration.dimensions, "dimension", "dimensions"),
        )));
    }
    let vector = normalize(&embedded.vector).map_err(|why| {
        Error::Endpoint(format!(
            "model {} embedded the passage {passage}, and {why}",
            declaration.model
        ))
    })?;
    Ok((embedded.digest, vector))
}

/// The refusal when the declaration no longer builds the vectors the index holds.
fn declaration_changed(n: &str, held: &Declaration, now: &Declaration) -> Error {
    Error::Conflict(format!(
        "the declaration of index `{n}` now builds {} vectors of model {} with {}, and the index \
         holds {} vectors of model {} with {}; {}",
        now.backend,
        now.model,
        count(now.dimensions, "dimension", "dimensions"),
        held.backend,
        held.model,
        count(held.dimensions, "dimension", "dimensions"),
        rebuild_hint(n)
    ))
}

/// The endpoint at [`INDEX_TEMPLATE`](crate::INDEX_TEMPLATE).
pub(crate) struct IndexEndpoint {
    pub(crate) inner: Arc<Inner>,
}

impl IndexEndpoint {
    fn describe_index(&self, inv: &Invocation<'_>, n: &str) -> Result<Representation> {
        let face = face(inv, &[TEXT, TURTLE], "an index")?;
        require(inv, &read_index_scope(n), &format!("reading index `{n}`"))?;
        inv.depends_on(change_thread(n));
        let indexes = self.inner.indexes.lock().expect("index lock");
        let Some(state) = indexes.get(n) else {
            return Err(Error::NotFound(format!(
                "index `{n}` is not built: add a member with `sink \
                 urn:vector:member:index:{n}:passage:{{p}}`, or build it empty with `sink \
                 {INDEX_PREFIX}{n}`"
            )));
        };
        let d = &state.declaration;
        let passages = state.backend.passages();
        let exact = state.backend.exact();
        let list = member_list_digest(&passages);
        let bytes = if face == TEXT {
            let digest = match &state.digest {
                Some(digest) => format!("model digest {digest}"),
                None => "no model digest yet: the index holds no vectors".to_string(),
            };
            format!(
                "index {n}\nbackend {}\n{}\nmodel {}\n{digest}\n{}\n{}\nmember list {list}\nk at \
                 most {}\n",
                d.backend,
                if exact { "exact" } else { "approximate" },
                d.model,
                count(d.dimensions, "dimension", "dimensions"),
                count(passages.len(), "member", "members"),
                d.max_k
            )
            .into_bytes()
        } else {
            let subject = named(&format!("{INDEX_PREFIX}{n}"))?;
            let mut triples = vec![
                Triple::new(subject.clone(), named(RDF_TYPE)?, term("Index")),
                Triple::new(
                    subject.clone(),
                    term("declaration"),
                    named(&format!("{DECLARATION_PREFIX}{n}"))?,
                ),
                Triple::new(
                    subject.clone(),
                    term("backend"),
                    Literal::new_simple_literal(&d.backend),
                ),
                Triple::new(subject.clone(), term("exact"), Literal::from(exact)),
                Triple::new(
                    subject.clone(),
                    term("model"),
                    Literal::new_simple_literal(&d.model),
                ),
                Triple::new(
                    subject.clone(),
                    term("dimensions"),
                    Literal::from(d.dimensions as i64),
                ),
                Triple::new(
                    subject.clone(),
                    term("memberCount"),
                    Literal::from(passages.len() as i64),
                ),
                Triple::new(
                    subject.clone(),
                    term("memberListDigest"),
                    Literal::new_simple_literal(&list),
                ),
                Triple::new(
                    subject.clone(),
                    term("maxK"),
                    Literal::from(i64::from(d.max_k)),
                ),
            ];
            if let Some(digest) = &state.digest {
                triples.push(Triple::new(
                    subject,
                    term("modelDigest"),
                    Literal::new_simple_literal(digest),
                ));
            }
            turtle(&triples, &format!("index `{n}`"))?
        };
        Ok(Representation::new(ReprType::new(face), bytes).cacheable())
    }

    async fn rebuild(&self, inv: &Invocation<'_>, n: &str) -> Result<Representation> {
        require(
            inv,
            &write_index_scope(n),
            &format!("rebuilding index `{n}`"),
        )?;
        if inv
            .inline_arg("content")
            .is_ok_and(|body| !body.iter().all(u8::is_ascii_whitespace))
        {
            return Err(Error::InvalidArgument {
                name: "content".to_string(),
                detail: format!(
                    "a rebuild of index `{n}` takes no body: it is built from its declaration, \
                     its member list and their embeddings"
                ),
            });
        }
        let declaration = declaration::read(inv, n).await?;
        let factory = self.inner.factory(&declaration.backend)?.clone();
        let members: Vec<String> = {
            let indexes = self.inner.indexes.lock().expect("index lock");
            indexes
                .get(n)
                .map(|s| s.backend.passages())
                .unwrap_or_default()
        };
        announce(inv, n).await?;
        let mut digest: Option<String> = None;
        let mut backend = factory(declaration.dimensions);
        for passage in &members {
            let (got, vector) = embed_passage(inv, &declaration, passage).await?;
            match &digest {
                Some(held) if *held != got => {
                    return Err(Error::Conflict(format!(
                        "rebuilding index `{n}` met two digests of model {}, {held} and {got}: the \
                         model changed during the rebuild; sink it again",
                        declaration.model
                    )))
                }
                _ => digest = Some(got),
            }
            backend.insert(passage, &vector);
        }
        let count_line = {
            let mut indexes = self.inner.indexes.lock().expect("index lock");
            let now: Vec<String> = indexes
                .get(n)
                .map(|s| s.backend.passages())
                .unwrap_or_default();
            if now != members {
                return Err(Error::Conflict(format!(
                    "the members of index `{n}` changed during the rebuild; sink it again"
                )));
            }
            let line = count(backend.len(), "member", "members");
            indexes.insert(
                n.to_string(),
                IndexState {
                    declaration: declaration.clone(),
                    digest: digest.clone(),
                    backend,
                },
            );
            line
        };
        announce(inv, n).await?;
        let digest = match digest {
            Some(d) => format!("digest {d}"),
            None => "no vectors".to_string(),
        };
        Ok(plain(format!(
            "rebuilt index {n}: {count_line}, model {} {digest}\n",
            declaration.model
        )))
    }
}

#[async_trait]
impl Endpoint for IndexEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let n = index_binding(inv)?;
        match inv.request.verb {
            Verb::Source => self.describe_index(inv, n),
            Verb::Sink => self.rebuild(inv, n).await,
            other => Err(Error::InvalidArgument {
                name: "verb".to_string(),
                detail: format!("an index answers source and sink, not {other:?}"),
            }),
        }
    }

    fn describe(&self) -> Description {
        Description::new(INDEX_ID)
            .title("Vector index")
            .summary(
                "Index {n}, a cache of its members' vectors held by the declared backend. source \
                 describes it, never its vectors: the backend, whether answers are exact, the \
                 model and the digest its vectors were made with, the dimensions, the member \
                 count, the digest of the member list and the bound on k (text/plain by default, \
                 as=text/turtle). sink with no body rebuilds it from its declaration, its member \
                 list and their embeddings, reading each passage under the caller's capability.",
            )
            .verb(Verb::Meta)
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("describe the index")
                    .requires(READ_INDEX)
                    .input(index_n_binding())
                    .output(TEXT)
                    .output(TURTLE),
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("rebuild the index from its declaration, members and embeddings")
                    .requires(WRITE_INDEX)
                    .requires(FS_READ)
                    .input(index_n_binding())
                    .output(TEXT),
            )
    }
}

/// The endpoint at [`MEMBER_TEMPLATE`](crate::MEMBER_TEMPLATE).
pub(crate) struct MemberEndpoint {
    pub(crate) inner: Arc<Inner>,
}

fn passage_binding<'a>(inv: &'a Invocation<'_>) -> Result<&'a str> {
    let p = inv
        .bindings
        .get("p")
        .ok_or_else(|| Error::MissingArgument("p".to_string()))?;
    if p.len() > 2048 || ikigai_core::Iri::parse(p).is_err() {
        return Err(Error::InvalidArgument {
            name: "p".to_string(),
            detail: format!("`{p}` is not a passage name: a passage is named by an IRI"),
        });
    }
    Ok(p)
}

impl MemberEndpoint {
    async fn add(&self, inv: &Invocation<'_>, n: &str, p: &str) -> Result<Representation> {
        require(
            inv,
            &write_index_scope(n),
            &format!("adding a member to index `{n}`"),
        )?;
        let declaration = declaration::read(inv, n).await?;
        self.inner.factory(&declaration.backend)?;
        {
            let indexes = self.inner.indexes.lock().expect("index lock");
            if let Some(state) = indexes.get(n) {
                if !state.declaration.same_vectors(&declaration) {
                    return Err(declaration_changed(n, &state.declaration, &declaration));
                }
            }
        }
        let (digest, vector) = embed_passage(inv, &declaration, p).await?;
        announce(inv, n).await?;
        let line = {
            let mut indexes = self.inner.indexes.lock().expect("index lock");
            let state = match indexes.get_mut(n) {
                Some(state) => state,
                None => {
                    let backend = self.inner.factory(&declaration.backend)?(declaration.dimensions);
                    indexes.entry(n.to_string()).or_insert(IndexState {
                        declaration: declaration.clone(),
                        digest: None,
                        backend,
                    })
                }
            };
            if !state.declaration.same_vectors(&declaration) {
                return Err(declaration_changed(n, &state.declaration, &declaration));
            }
            match &state.digest {
                Some(held) if *held != digest => {
                    return Err(digest_mismatch(n, &declaration.model, held, &digest))
                }
                Some(_) => {}
                None => state.digest = Some(digest),
            }
            // The k bound is not part of the vectors: a changed one takes effect here.
            state.declaration = declaration;
            let verb = if state.backend.contains(p) {
                "refreshed"
            } else {
                "added"
            };
            state.backend.insert(p, &vector);
            let preposition = if verb == "added" { "to" } else { "in" };
            format!(
                "{verb} {p} {preposition} index {n}: {}\n",
                count(state.backend.len(), "member", "members")
            )
        };
        announce(inv, n).await?;
        Ok(plain(line))
    }

    async fn remove(&self, inv: &Invocation<'_>, n: &str, p: &str) -> Result<Representation> {
        require(
            inv,
            &write_index_scope(n),
            &format!("removing a member from index `{n}`"),
        )?;
        let held = {
            let indexes = self.inner.indexes.lock().expect("index lock");
            indexes
                .get(n)
                .map(|s| (s.backend.contains(p), s.backend.len()))
        };
        // A delete is idempotent: removing what is not there changes nothing and says so.
        if !matches!(held, Some((true, _))) {
            let len = held.map_or(0, |(_, len)| len);
            return Ok(plain(format!(
                "{p} is not a member of index {n}: {}\n",
                count(len, "member", "members")
            )));
        }
        announce(inv, n).await?;
        let line = {
            let mut indexes = self.inner.indexes.lock().expect("index lock");
            let len = match indexes.get_mut(n) {
                Some(state) => {
                    state.backend.remove(p);
                    if state.backend.is_empty() {
                        state.digest = None;
                    }
                    state.backend.len()
                }
                None => 0,
            };
            format!(
                "removed {p} from index {n}: {}\n",
                count(len, "member", "members")
            )
        };
        announce(inv, n).await?;
        Ok(plain(line))
    }

    fn exists(&self, inv: &Invocation<'_>, n: &str, p: &str) -> Result<Representation> {
        require(inv, &read_index_scope(n), &format!("reading index `{n}`"))?;
        inv.depends_on(change_thread(n));
        let held = {
            let indexes = self.inner.indexes.lock().expect("index lock");
            indexes.get(n).is_some_and(|s| s.backend.contains(p))
        };
        Ok(plain(if held { "true" } else { "false" }.to_string()).cacheable())
    }
}

#[async_trait]
impl Endpoint for MemberEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let n = index_binding(inv)?;
        let p = passage_binding(inv)?;
        match inv.request.verb {
            Verb::Sink => self.add(inv, n, p).await,
            Verb::Delete => self.remove(inv, n, p).await,
            Verb::Exists => self.exists(inv, n, p),
            other => Err(Error::InvalidArgument {
                name: "verb".to_string(),
                detail: format!("a member answers sink, delete and exists, not {other:?}"),
            }),
        }
    }

    fn describe(&self) -> Description {
        let bindings = || {
            [
                index_n_binding(),
                ArgSpec::new("p")
                    .summary("the passage's name, an IRI the caller may read")
                    .class(XSD_ANY_URI)
                    .binding(),
            ]
        };
        let action = |verb: Verb, summary: &str, requires: &[&str]| {
            let mut spec = ActionSpec::new(verb).summary(summary).output(TEXT);
            for r in requires {
                spec = spec.requires(*r);
            }
            for b in bindings() {
                spec = spec.input(b);
            }
            spec
        };
        Description::new(MEMBER_ID)
            .title("Vector index member")
            .summary(
                "Passage {p} in index {n}. sink reads the passage's text through the kernel under \
                 the caller's capability, embeds it with the index's model and inserts it; a \
                 digest other than the index's is refused, naming both. delete removes it at \
                 once, and no nearest answers it after. exists says whether it is in the index. \
                 Each change cuts the index's thread. The text is never kept: the index holds the \
                 name and the vector.",
            )
            .verb(Verb::Meta)
            .action(action(
                Verb::Sink,
                "add the passage to the index",
                &[WRITE_INDEX, FS_READ],
            ))
            .action(action(
                Verb::Delete,
                "remove the passage from the index",
                &[WRITE_INDEX],
            ))
            .action(action(
                Verb::Exists,
                "whether the passage is in the index",
                &[READ_INDEX],
            ))
    }
}

/// The endpoint at [`CHANGE_TEMPLATE`](crate::CHANGE_TEMPLATE): a `sink` that changes nothing
/// and is written to so the kernel cuts the thread named after it.
///
/// It takes the grant that writes the index. ⚠ The kernel still cuts the target of a write that
/// ran and was refused, so a caller holding a write grant for some OTHER index costs this
/// index's readers one recomputation (as ikigai-store's graph-written door says of itself).
pub(crate) struct ChangeEndpoint;

#[async_trait]
impl Endpoint for ChangeEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let n = index_binding(inv)?;
        match inv.request.verb {
            Verb::Sink => {
                require(
                    inv,
                    &write_index_scope(n),
                    &format!("invalidating cached reads of index `{n}`"),
                )?;
                Ok(plain(format!("invalidated cached reads of index {n}\n")))
            }
            other => Err(Error::InvalidArgument {
                name: "verb".to_string(),
                detail: format!("a change thread answers sink, not {other:?}"),
            }),
        }
    }

    fn describe(&self) -> Description {
        Description::new(CHANGE_ID)
            .title("Vector index change thread")
            .summary(
                "Written by every member change and rebuild of index {n}, so the kernel cuts the \
                 thread named after it, which nearest, the index description and a member's \
                 exists hang from. A sink here changes no data; it takes \
                 urn:cap:vector:write:index:name:{n}.",
            )
            .verb(Verb::Meta)
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("cut the index's thread")
                    .requires(WRITE_INDEX)
                    .input(index_n_binding())
                    .output(TEXT),
            )
    }
}
