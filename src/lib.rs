//! Vector search as ikigai resources: one contract, pluggable backends.
//!
//! Given a question's text, this module answers the **names** of the nearest passages, never
//! their text, so reading what a name holds stays under the caller's own capability. The
//! README is the resource map; `docs/design.md` is the design it follows.
//!
//! # The names
//!
//! | Name | Verbs | What it is |
//! |---|---|---|
//! | `urn:vector:declaration:index:{n}` | `source` | the index's declaration: backend, embedding model, dimensions, the `k` bound |
//! | `urn:vector:index:name:{n}` | `source`, `sink` | the index: `source` describes it (never its vectors), `sink` with no body rebuilds it |
//! | `urn:vector:member:index:{n}:passage:{p}` | `sink`, `delete`, `exists` | one passage in the index |
//! | `urn:vector:embedding:model:{m}:hash:{h}` | `source` | a passage text's embedding, through `urn:llm:embed` |
//! | `urn:vector:nearest:index:{n}` | `source` | the `k` nearest passages to `text`, by name |
//! | `urn:vector:change:index:{n}` | `sink` | written by every member change and rebuild, so the kernel cuts the index's thread |
//!
//! # Where a declaration lives
//!
//! The declaration of index `{n}` is the Turtle file `<config home>/vector/index/{n}.ttl`, read
//! **through the kernel** as [`FILE_PREFIX`]`index/{n}.ttl`: this crate opens no file. The host
//! binds a file module at [`FILE_TEMPLATE`], jailed to the config home's `vector` directory, the
//! way ikigai-knowledge reads `urn:knowledge:file:path:{path}`. A declaration names its own
//! subject `<>`:
//!
//! ```text
//! @prefix vector: <https://ikigai-rs.dev/ns/vector#> .
//! <> a vector:IndexDeclaration ;
//!    vector:backend "flat" ;
//!    vector:model "embeddinggemma" ;
//!    vector:provider "ollama" ;
//!    vector:dimensions 768 ;
//!    vector:maxK 50 .
//! ```
//!
//! `vector:model` and `vector:dimensions` are required. `vector:backend` defaults to `flat`,
//! `vector:maxK` to 50, and with no `vector:provider` `urn:llm:embed` uses the host's
//! configured default.
//!
//! # Authority
//!
//! Per index, singular and keyed; no token spans indexes. Each endpoint declares the family and
//! refuses any other index by its own rule, with a typed `Denied`, before it reads anything:
//!
//! - [`READ_INDEX_PREFIX`]`{n}`: `nearest`, the index description, a member's `exists`;
//! - [`WRITE_INDEX_PREFIX`]`{n}`: a member's `sink` and `delete`, and the rebuild;
//! - [`READ_DECLARATION_PREFIX`]`{n}`: the declaration.
//!
//! Reading the declaration file is the caller's read too, so the endpoints that read it also
//! declare [`FS_READ`], which the conventional mount (`ikigai-fs`) judges by path. **Reading a
//! passage is never granted here**: a member `sink` reads the passage under the caller's own
//! capability, and so does every reader who sources a name `nearest` answered.
//!
//! # Golden threads
//!
//! The kernel cuts the thread named after a write's target, and an endpoint cannot cut another
//! one. So every member change and every rebuild also writes [`CHANGE_PREFIX`]`{n}` (as
//! `ikigai-store` writes `urn:iki:store:graph-written:{graph}`), and `nearest`, the index
//! description and a member's `exists` hang from that thread. `nearest` is cheap and is
//! recomputed on every change; **a composite that wants it takes its names as a VALUE**, never
//! joins it (design, section 3).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ikigai_core::{
    Denial, EndpointSpace, Error, Invocation, Iri, ReprType, Representation, Request, Result,
    UriTemplate, Verb,
};
use oxrdf::{NamedNode, Triple};
use oxttl::TurtleSerializer;
use sha2::{Digest, Sha256};

mod backend;
mod declaration;
mod embedding;
mod index;
mod nearest;

pub use backend::{normalize, rank, Backend, BackendFactory, Flat, Hit, Score, FLAT};
pub use declaration::{Declaration, DEFAULT_MAX_K, MAX_K_LIMIT};

use declaration::DeclarationEndpoint;
use embedding::EmbeddingEndpoint;
use index::{ChangeEndpoint, IndexEndpoint, MemberEndpoint};
use nearest::NearestEndpoint;

/// The namespace of this module's own vocabulary, [`VOCABULARY`].
pub const VOCABULARY_NAMESPACE: &str = "https://ikigai-rs.dev/ns/vector#";

/// This module's own vocabulary, as Turtle (`src/vocabulary.ttl`): an `owl:Ontology` for
/// `https://ikigai-rs.dev/ns/vector` whose `owl:versionInfo` is the crate version, defining
/// every term under [`VOCABULARY_NAMESPACE`] a declaration or an answer uses. Not deployed yet.
pub const VOCABULARY: &str = include_str!("vocabulary.ttl");

/// The template the index declaration is served at.
pub const DECLARATION_TEMPLATE: &str = "urn:vector:declaration:index:{n}";
/// The prefix of [`DECLARATION_TEMPLATE`].
pub const DECLARATION_PREFIX: &str = "urn:vector:declaration:index:";
/// The declaration endpoint's description id.
pub const DECLARATION_ID: &str = "vector-declaration";

/// The template the index is served at.
pub const INDEX_TEMPLATE: &str = "urn:vector:index:name:{n}";
/// The prefix of [`INDEX_TEMPLATE`].
pub const INDEX_PREFIX: &str = "urn:vector:index:name:";
/// The index endpoint's description id.
pub const INDEX_ID: &str = "vector-index";

/// The template a member edge is served at.
pub const MEMBER_TEMPLATE: &str = "urn:vector:member:index:{n}:passage:{p}";
/// The prefix of [`MEMBER_TEMPLATE`].
pub const MEMBER_PREFIX: &str = "urn:vector:member:index:";
/// The member endpoint's description id.
pub const MEMBER_ID: &str = "vector-member";

/// The template an embedding is served at.
pub const EMBEDDING_TEMPLATE: &str = "urn:vector:embedding:model:{m}:hash:{h}";
/// The prefix of [`EMBEDDING_TEMPLATE`].
pub const EMBEDDING_PREFIX: &str = "urn:vector:embedding:model:";
/// The embedding endpoint's description id.
pub const EMBEDDING_ID: &str = "vector-embedding";

/// The template nearest is served at (`text=`, `k=`).
pub const NEAREST_TEMPLATE: &str = "urn:vector:nearest:index:{n}";
/// The prefix of [`NEAREST_TEMPLATE`].
pub const NEAREST_PREFIX: &str = "urn:vector:nearest:index:";
/// The nearest endpoint's description id.
pub const NEAREST_ID: &str = "vector-nearest";

/// The template of an index's change thread: a `sink` there does nothing but be written to, so
/// the kernel cuts the thread named after it.
pub const CHANGE_TEMPLATE: &str = "urn:vector:change:index:{n}";
/// The prefix of [`CHANGE_TEMPLATE`].
pub const CHANGE_PREFIX: &str = "urn:vector:change:index:";
/// The change endpoint's description id.
pub const CHANGE_ID: &str = "vector-change";

/// Where the declaration is read: the template a host binds a file module at, jailed to the
/// config home's `vector` directory.
pub const FILE_TEMPLATE: &str = "urn:vector:file:path:{path}";
/// The prefix of [`FILE_TEMPLATE`].
pub const FILE_PREFIX: &str = "urn:vector:file:path:";

/// The embedding endpoint a host binds (ikigai-llm's).
pub const LLM_EMBED: &str = "urn:llm:embed";

/// The scope that reads index `{n}` is this followed by the name.
pub const READ_INDEX_PREFIX: &str = "urn:cap:vector:read:index:name:";
/// What the reading endpoints declare: some index read grant.
pub const READ_INDEX: &str = "urn:cap:vector:read:index:name:*";
/// The scope that writes index `{n}` is this followed by the name.
pub const WRITE_INDEX_PREFIX: &str = "urn:cap:vector:write:index:name:";
/// What the writing endpoints declare: some index write grant.
pub const WRITE_INDEX: &str = "urn:cap:vector:write:index:name:*";
/// The scope that reads the declaration of index `{n}` is this followed by the name.
pub const READ_DECLARATION_PREFIX: &str = "urn:cap:vector:read:declaration:index:";
/// What the declaration endpoint declares: some declaration read grant.
pub const READ_DECLARATION: &str = "urn:cap:vector:read:declaration:index:*";
/// What reading the declaration file takes under `ikigai-fs`: some read grant, judged by path
/// where the file is served.
pub const FS_READ: &str = "urn:cap:fs:read:*";

pub(crate) const TEXT: &str = "text/plain";
pub(crate) const TURTLE: &str = "text/turtle";
pub(crate) const JSON: &str = "application/json";
pub(crate) const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
pub(crate) const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
pub(crate) const XSD_ANY_URI: &str = "http://www.w3.org/2001/XMLSchema#anyURI";

/// The module's state and the backends a host offers. Clone it to share it: every space built
/// from one `Vectors` serves the same indexes.
///
/// An index is a **cache**, never a source of truth (design, section 1): it lives in this
/// process, and a host that restarts re-adds its members (their embeddings come back from the
/// kernel's cache while it holds them, or from the model).
///
/// ```
/// use std::sync::Arc;
/// use ikigai_core::{Fallback, Kernel, Space};
///
/// let vectors = ikigai_vector::Vectors::new();
/// let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(vectors.space())];
/// let kernel = Kernel::new(Arc::new(Fallback::new(spaces)));
/// # let _ = kernel;
/// ```
#[derive(Clone)]
pub struct Vectors {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    backends: BTreeMap<String, BackendFactory>,
    pub(crate) indexes: Mutex<BTreeMap<String, IndexState>>,
}

/// One built index: the declaration it was built from, the model digest of its vectors, and the
/// backend holding them.
pub(crate) struct IndexState {
    pub(crate) declaration: Declaration,
    pub(crate) digest: Option<String>,
    pub(crate) backend: Box<dyn Backend>,
}

impl Default for Vectors {
    fn default() -> Self {
        Vectors::new()
    }
}

impl Vectors {
    /// No index yet, and the one backend this crate ships, [`Flat`] under [`FLAT`].
    pub fn new() -> Vectors {
        Vectors {
            inner: Arc::new(Inner {
                backends: BTreeMap::from([(FLAT.to_string(), Flat::factory())]),
                indexes: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Offer another backend kind, which a declaration then names with `vector:backend`. Call
    /// it before building a space: it replaces the shared state.
    pub fn with_backend(self, kind: &str, factory: BackendFactory) -> Vectors {
        let mut backends = self.inner.backends.clone();
        backends.insert(kind.to_string(), factory);
        Vectors {
            inner: Arc::new(Inner {
                backends,
                indexes: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// The backend kinds this host offers, ascending.
    pub fn backends(&self) -> Vec<String> {
        self.inner.backends.keys().cloned().collect()
    }

    /// Everything index `n`'s backend holds ([`Backend::entries`]): passage names and vectors,
    /// ascending by name, or `None` before the index is built. For a host's diagnostics and for
    /// the backend conformance test, which checks against it that no text is kept; never served.
    pub fn held(&self, n: &str) -> Option<Vec<(String, Vec<f32>)>> {
        let indexes = self.inner.indexes.lock().expect("index lock");
        indexes.get(n).map(|state| state.backend.entries())
    }

    /// The `urn:vector:*` space over this state.
    ///
    /// Host-named: it serves the state it was handed, so it claims no name. A host binds the
    /// file mount (see the crate docs) and ikigai-llm beside it, never inside it.
    pub fn space(&self) -> EndpointSpace {
        let template = |t: &str| UriTemplate::parse(t).expect("a valid template");
        EndpointSpace::new()
            .bind(template(DECLARATION_TEMPLATE), DeclarationEndpoint)
            .bind(
                template(INDEX_TEMPLATE),
                IndexEndpoint {
                    inner: self.inner.clone(),
                },
            )
            .bind(
                template(MEMBER_TEMPLATE),
                MemberEndpoint {
                    inner: self.inner.clone(),
                },
            )
            .bind(template(EMBEDDING_TEMPLATE), EmbeddingEndpoint)
            .bind(
                template(NEAREST_TEMPLATE),
                NearestEndpoint {
                    inner: self.inner.clone(),
                },
            )
            .bind(template(CHANGE_TEMPLATE), ChangeEndpoint)
    }
}

impl Inner {
    pub(crate) fn factory(&self, kind: &str) -> Result<&BackendFactory> {
        self.backends.get(kind).ok_or_else(|| {
            let offered: Vec<&str> = self.backends.keys().map(String::as_str).collect();
            Error::Endpoint(format!(
                "the backend `{kind}` is not one this host offers: it offers {}",
                offered.join(", ")
            ))
        })
    }
}

/// The scope that reads index `n`.
///
/// ```
/// assert_eq!(ikigai_vector::read_index_scope("docs"), "urn:cap:vector:read:index:name:docs");
/// ```
pub fn read_index_scope(n: &str) -> String {
    format!("{READ_INDEX_PREFIX}{n}")
}

/// The scope that writes index `n`.
///
/// ```
/// assert_eq!(ikigai_vector::write_index_scope("docs"), "urn:cap:vector:write:index:name:docs");
/// ```
pub fn write_index_scope(n: &str) -> String {
    format!("{WRITE_INDEX_PREFIX}{n}")
}

/// The scope that reads the declaration of index `n`.
///
/// ```
/// assert_eq!(
///     ikigai_vector::read_declaration_scope("docs"),
///     "urn:cap:vector:read:declaration:index:docs"
/// );
/// ```
pub fn read_declaration_scope(n: &str) -> String {
    format!("{READ_DECLARATION_PREFIX}{n}")
}

/// The file the declaration of index `n` is read from, relative to the config home's `vector`
/// directory.
///
/// ```
/// assert_eq!(
///     ikigai_vector::declaration_file("docs").unwrap(),
///     "urn:vector:file:path:index/docs.ttl"
/// );
/// // An index name is one path segment: nothing that could leave the directory.
/// assert!(ikigai_vector::declaration_file("../elsewhere").is_err());
/// ```
pub fn declaration_file(n: &str) -> Result<String> {
    check_index_name(n)?;
    Ok(format!("{FILE_PREFIX}index/{n}.ttl"))
}

/// The name of the embedding of `text` by `model`: `{h}` is the SHA-256 of the text's UTF-8
/// bytes, in lowercase hex.
///
/// ```
/// assert_eq!(
///     ikigai_vector::embedding_name("embeddinggemma", "abc"),
///     "urn:vector:embedding:model:embeddinggemma:hash:\
///      ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
/// );
/// ```
pub fn embedding_name(model: &str, text: &str) -> String {
    format!(
        "{EMBEDDING_PREFIX}{model}:hash:{}",
        sha256_hex(text.as_bytes())
    )
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// An index name is one ordinary path segment and a plain IRI segment.
pub(crate) fn check_index_name(n: &str) -> Result<()> {
    let ok = !n.is_empty()
        && !n.starts_with('.')
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidArgument {
            name: "n".to_string(),
            detail: format!(
                "`{n}` is not an index name: use 1 to 64 letters, digits, `-`, `_` and `.`, not \
                 starting with `.` (the file vector/index/{{n}}.ttl in the config home)"
            ),
        })
    }
}

/// The index name a request's `{n}` binding carries, checked.
pub(crate) fn index_binding<'a>(inv: &'a Invocation<'_>) -> Result<&'a str> {
    let n = inv
        .bindings
        .get("n")
        .ok_or_else(|| Error::MissingArgument("n".to_string()))?;
    check_index_name(n)?;
    Ok(n)
}

/// The module's own rule, before anything is read: a caller without `scope` learns nothing.
pub(crate) fn require(inv: &Invocation<'_>, scope: &str, doing: &str) -> Result<()> {
    if inv.capability.allows(scope) {
        Ok(())
    } else {
        Err(Error::Denied(Denial::from(format!(
            "{doing} takes `{scope}`, which this capability does not hold"
        ))))
    }
}

pub(crate) fn iri(s: &str) -> Result<Iri> {
    Iri::parse(s).map_err(|e| Error::Endpoint(format!("`{s}` is not an IRI: {e}")))
}

pub(crate) fn named(s: &str) -> Result<NamedNode> {
    NamedNode::new(s).map_err(|e| Error::Endpoint(format!("`{s}` is not an IRI: {e}")))
}

pub(crate) fn plain(text: String) -> Representation {
    Representation::new(ReprType::new(TEXT), text.into_bytes())
}

/// `n` and the plural noun, agreeing: `1 member`, `2 members`.
pub(crate) fn count(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

/// The face `as=` asks for, one of `offered`; none asks for the first.
pub(crate) fn face(
    inv: &Invocation<'_>,
    offered: &[&'static str],
    what: &str,
) -> Result<&'static str> {
    let Ok(asked) = inv.inline_str("as") else {
        return Ok(offered[0]);
    };
    let essence = ikigai_core::media_type_essence(asked.trim());
    if essence.is_empty() {
        return Ok(offered[0]);
    }
    offered
        .iter()
        .find(|o| **o == essence)
        .copied()
        .ok_or_else(|| Error::InvalidArgument {
            name: "as".to_string(),
            detail: format!(
                "`{essence}` is not a face of {what}: ask for {} (the first is the default)",
                offered.join(", ")
            ),
        })
}

/// Turtle written by the RDF library: the `vector:` prefix (and `xsd:` when asked), then the
/// triples.
pub(crate) fn turtle(triples: &[Triple], what: &str) -> Result<Vec<u8>> {
    let failed = |e: std::io::Error| Error::Endpoint(format!("{what}: writing Turtle: {e}"));
    let serializer = TurtleSerializer::new()
        .with_prefix("vector", VOCABULARY_NAMESPACE)
        .and_then(|s| s.with_prefix("xsd", "http://www.w3.org/2001/XMLSchema#"))
        .map_err(|e| Error::Endpoint(format!("{what}: {e}")))?;
    let mut writer = serializer.for_writer(Vec::new());
    for t in triples {
        writer.serialize_triple(t).map_err(failed)?;
    }
    writer.finish().map_err(failed)
}

/// A term of this module's vocabulary.
pub(crate) fn term(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{VOCABULARY_NAMESPACE}{local}"))
}

/// Write index `n`'s change thread: a sub-request `sink` to [`CHANGE_PREFIX`]`{n}`, so the kernel
/// cuts the thread `nearest`, the description and `exists` hang from.
///
/// ★ Called before AND after a change, as ikigai-store announces a graph write: every refusal
/// the first call can meet (the nesting budget, a floor) is decided before dispatch and comes
/// out the same the second time, so a change that could not be announced is refused before it
/// happens. The second call cuts anything a reader cached between the first and the change.
///
/// A detached invocation has no kernel and so no cache: nothing to invalidate.
pub(crate) async fn announce(inv: &Invocation<'_>, n: &str) -> Result<()> {
    let target = iri(&format!("{CHANGE_PREFIX}{n}"))?;
    match inv.issue(Request::new(Verb::Sink, target)).await {
        Err(Error::Endpoint(detail)) if detail == "sub-requests require a kernel context" => Ok(()),
        other => other.map(drop),
    }
}
