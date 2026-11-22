//! A host for the tests: the module's space beside three stubs a real host fills with its own
//! modules. Everything is in memory, so no test opens a file or a socket.
//!
//! - the file mount at `urn:vector:file:path:{path}` (a host binds `ikigai-fs` over the config
//!   home's `vector` directory), serving declarations from a map;
//! - passages at `urn:example:passage:{id}`, serving text from a map;
//! - `urn:llm:embed` (a host binds ikigai-llm), deterministic: a text `vec:a,b,c,d` embeds as
//!   those values, any other text as four values drawn from its SHA-256, and the digest is
//!   whatever the test set. Each stub is cacheable and hangs from a thread a test cuts when it
//!   changes the stub's state, the way a host's watcher would.
#![allow(dead_code)] // each test binary uses part of the host

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, EndpointSpace, Error, Exact, Fallback, FnEndpoint,
    Iri, Kernel, ReprType, Representation, Request, Space, UriTemplate, Verb,
};
use ikigai_vector::{BackendFactory, Vectors};

pub const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
/// The thread the embed stub hangs from, cut when a test changes the model's digest.
pub const INSTALLED: &str = "urn:llm:test:installed";
pub const DIGEST: &str = "sha256:aaaa1111";
pub const OTHER_DIGEST: &str = "sha256:bbbb2222";

/// A declaration of a flat index of model `m`, 4 dimensions.
pub fn declaration(backend: &str, max_k: Option<u32>) -> String {
    let max_k = max_k
        .map(|k| format!(" ; vector:maxK {k}"))
        .unwrap_or_default();
    format!(
        "@prefix vector: <https://ikigai-rs.dev/ns/vector#> .\n\
         <> a vector:IndexDeclaration ; vector:backend \"{backend}\" ; vector:model \"m\" ; \
         vector:dimensions 4{max_k} .\n"
    )
}

pub fn passage(id: &str) -> String {
    format!("urn:example:passage:{id}")
}

pub struct Host {
    pub vectors: Vectors,
    pub kernel: Kernel,
    pub files: Arc<Mutex<BTreeMap<String, String>>>,
    pub passages: Arc<Mutex<BTreeMap<String, String>>>,
    pub digest: Arc<Mutex<String>>,
    /// Every text the embed stub was handed, in order.
    pub embedded: Arc<Mutex<Vec<String>>>,
    pub embeds: Arc<AtomicUsize>,
}

impl Host {
    /// A host offering `flat`, with index `a` and index `b` declared flat.
    pub fn new() -> Host {
        Host::with(Vectors::new(), "flat")
    }

    /// A host over `vectors`, with indexes `a` and `b` declared with `backend`.
    pub fn with(vectors: Vectors, backend: &str) -> Host {
        let files = Arc::new(Mutex::new(BTreeMap::from([
            ("index/a.ttl".to_string(), declaration(backend, Some(5))),
            ("index/b.ttl".to_string(), declaration(backend, None)),
        ])));
        let passages = Arc::new(Mutex::new(BTreeMap::<String, String>::new()));
        let digest = Arc::new(Mutex::new(DIGEST.to_string()));
        let embedded = Arc::new(Mutex::new(Vec::new()));
        let embeds = Arc::new(AtomicUsize::new(0));

        let file = {
            let files = files.clone();
            FnEndpoint::new("file", move |inv| {
                let path = inv.bindings.get("path").unwrap_or_default();
                match files.lock().unwrap().get(path) {
                    Some(text) => Ok(Representation::new(
                        ReprType::new("text/turtle"),
                        text.as_bytes().to_vec(),
                    )
                    .cacheable()),
                    None => Err(Error::NotFound(format!("no file {path}"))),
                }
            })
            .with_description(
                Description::new("file")
                    .verb(Verb::Source)
                    .input(ArgSpec::new("path").class(XSD_STRING).binding())
                    .output("text/turtle"),
            )
        };
        let text_of = {
            let passages = passages.clone();
            FnEndpoint::new("passage", move |inv| {
                let id = inv.bindings.get("id").unwrap_or_default();
                match passages.lock().unwrap().get(id) {
                    Some(text) => Ok(Representation::new(
                        ReprType::new("text/plain"),
                        text.as_bytes().to_vec(),
                    )
                    .cacheable()),
                    None => Err(Error::NotFound(format!("no passage {id}"))),
                }
            })
            .with_description(
                Description::new("passage")
                    .verb(Verb::Source)
                    .input(ArgSpec::new("id").class(XSD_STRING).binding())
                    .output("text/plain"),
            )
        };
        let embed = {
            let digest = digest.clone();
            let embedded = embedded.clone();
            let embeds = embeds.clone();
            FnEndpoint::new("embed-stub", move |inv| {
                let model = inv.inline_str("model")?.to_string();
                let text = inv.inline_str("text")?.to_string();
                embeds.fetch_add(1, Ordering::SeqCst);
                embedded.lock().unwrap().push(text.clone());
                let vector: Vec<f64> = match text.strip_prefix("vec:") {
                    Some(values) => values
                        .split(',')
                        .map(|v| v.trim().parse::<f64>().expect("a number"))
                        .collect(),
                    None => {
                        let hash = ikigai_vector::embedding_name("x", &text);
                        let hex = hash.rsplit(':').next().unwrap();
                        (0..4)
                            .map(|i| {
                                let byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
                                f64::from(byte) - 127.5
                            })
                            .collect()
                    }
                };
                let body = serde_json::json!({
                    "vector": vector,
                    "model": format!("{model}:latest"),
                    "digest": digest.lock().unwrap().clone(),
                    "dimensions": vector.len(),
                });
                Ok(Representation::new(
                    ReprType::new("application/json"),
                    serde_json::to_vec(&body).unwrap(),
                )
                .cacheable()
                .depends_on(INSTALLED))
            })
            .with_description(
                Description::new("embed-stub")
                    .verb(Verb::Source)
                    .input(ArgSpec::new("model").class(XSD_STRING))
                    .input(ArgSpec::new("text").class(XSD_STRING))
                    .input(ArgSpec::new("provider").class(XSD_STRING).optional())
                    .output("application/json"),
            )
        };
        let mounts = EndpointSpace::new()
            .bind(
                UriTemplate::parse(ikigai_vector::FILE_TEMPLATE).unwrap(),
                file,
            )
            .bind(
                UriTemplate::parse("urn:example:passage:{id}").unwrap(),
                text_of,
            )
            .bind(Exact::new(ikigai_vector::LLM_EMBED), embed);
        let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(vectors.space()), Arc::new(mounts)];
        let kernel = Kernel::new(Arc::new(Fallback::new(spaces)));
        Host {
            vectors,
            kernel,
            files,
            passages,
            digest,
            embedded,
            embeds,
        }
    }

    pub fn put_passage(&self, id: &str, text: &str) {
        self.passages
            .lock()
            .unwrap()
            .insert(id.to_string(), text.to_string());
        self.kernel.cut(passage(id));
    }

    pub fn set_digest(&self, digest: &str) {
        *self.digest.lock().unwrap() = digest.to_string();
        self.kernel.cut(INSTALLED);
    }

    pub fn declare(&self, n: &str, turtle: &str) {
        let path = format!("index/{n}.ttl");
        self.files
            .lock()
            .unwrap()
            .insert(path.clone(), turtle.to_string());
        self.kernel
            .cut(format!("{}{path}", ikigai_vector::FILE_PREFIX));
    }

    pub fn issue(&self, request: Request, cap: &Capability) -> Result<Representation, Error> {
        futures::executor::block_on(self.kernel.issue(request, cap))
    }

    pub fn add(&self, n: &str, id: &str, cap: &Capability) -> Result<String, Error> {
        self.issue(member(Verb::Sink, n, id), cap).map(|r| text(&r))
    }

    pub fn remove(&self, n: &str, id: &str, cap: &Capability) -> Result<String, Error> {
        self.issue(member(Verb::Delete, n, id), cap)
            .map(|r| text(&r))
    }

    pub fn rebuild(&self, n: &str, cap: &Capability) -> Result<String, Error> {
        self.issue(
            request(
                Verb::Sink,
                &format!("{}{n}", ikigai_vector::INDEX_PREFIX),
                &[],
            ),
            cap,
        )
        .map(|r| text(&r))
    }

    pub fn nearest(
        &self,
        n: &str,
        query: &str,
        k: &str,
        face: Option<&str>,
        cap: &Capability,
    ) -> Result<String, Error> {
        let mut args = vec![("text", query), ("k", k)];
        if let Some(face) = face {
            args.push(("as", face));
        }
        self.issue(
            request(
                Verb::Source,
                &format!("{}{n}", ikigai_vector::NEAREST_PREFIX),
                &args,
            ),
            cap,
        )
        .map(|r| text(&r))
    }
}

pub fn member(verb: Verb, n: &str, id: &str) -> Request {
    request(
        verb,
        &format!(
            "{}{n}:passage:{}",
            ikigai_vector::MEMBER_PREFIX,
            passage(id)
        ),
        &[],
    )
}

/// A request with inline arguments.
pub fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, Iri::parse(iri).expect("a valid IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

pub fn text(repr: &Representation) -> String {
    String::from_utf8(repr.bytes.to_vec()).expect("UTF-8")
}

/// The fs grant the endpoints that read a declaration declare (`urn:cap:fs:read:*`); the stub
/// mount judges nothing, a real `ikigai-fs` judges the path.
pub const FS: &str = "urn:cap:fs:read:vector";

/// Exactly the published grants that write index `n`.
pub fn writer(n: &str) -> Capability {
    Capability::scoped([ikigai_vector::write_index_scope(n), FS.to_string()])
}

/// Exactly the published grant that reads index `n`.
pub fn reader(n: &str) -> Capability {
    Capability::scoped([ikigai_vector::read_index_scope(n)])
}

/// A factory the backend test can wrap.
pub fn flat_factory() -> BackendFactory {
    ikigai_vector::Flat::factory()
}
