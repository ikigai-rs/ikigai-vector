//! The index declaration, `urn:vector:declaration:index:{n}`: a config ATOM, Turtle read
//! through the kernel from the host-bound file `urn:vector:file:path:index/{n}.ttl`.

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, Error, Invocation, ReprType, Representation, Result, Verb,
};
use oxrdf::{Literal, NamedOrBlankNode, Term, Triple};
use oxttl::TurtleParser;

use crate::{
    count, declaration_file, face, index_binding, iri, named, read_declaration_scope, require,
    term, turtle, DECLARATION_ID, DECLARATION_PREFIX, FILE_TEMPLATE, FLAT, FS_READ,
    READ_DECLARATION, TEXT, TURTLE, VOCABULARY_NAMESPACE, XSD_STRING,
};

/// The `k` bound when a declaration names none (design, section 1).
pub const DEFAULT_MAX_K: u32 = 50;
/// The largest `k` bound a declaration may set.
pub const MAX_K_LIMIT: u32 = 10_000;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// A parsed index declaration: what the index is built from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    /// The index's name.
    pub index: String,
    /// The backend kind (`flat` unless the declaration says otherwise).
    pub backend: String,
    /// The embedding model, as ikigai-llm installs it.
    pub model: String,
    /// The embedding provider, or `None` for the host's configured default.
    pub provider: Option<String>,
    /// How many values the model is expected to produce.
    pub dimensions: usize,
    /// The largest `k` nearest accepts.
    pub max_k: u32,
    /// Every statement the file holds, in file order, with `<>` resolved.
    pub(crate) triples: Vec<Triple>,
}

impl Declaration {
    /// Parse the declaration of index `n` from its Turtle, `<>` naming the declaration itself
    /// (`urn:vector:declaration:index:{n}`). A blank node is refused, so is a malformed value.
    ///
    /// ```
    /// let d = ikigai_vector::Declaration::parse(
    ///     "docs",
    ///     br#"@prefix vector: <https://ikigai-rs.dev/ns/vector#> .
    ///         <> vector:model "embeddinggemma" ; vector:dimensions 768 ."#,
    /// )
    /// .unwrap();
    /// assert_eq!((d.backend.as_str(), d.model.as_str(), d.dimensions), ("flat", "embeddinggemma", 768));
    /// assert_eq!((d.provider, d.max_k), (None, 50));
    /// ```
    pub fn parse(n: &str, bytes: &[u8]) -> Result<Declaration> {
        let base = format!("{DECLARATION_PREFIX}{n}");
        let path = format!("vector/index/{n}.ttl");
        let refuse =
            |why: String| Error::Endpoint(format!("the declaration of index `{n}`: {why}"));
        let mut parser = TurtleParser::new()
            .with_base_iri(base.as_str())
            .map_err(|e| refuse(e.to_string()))?
            .for_slice(bytes);
        let mut triples = Vec::new();
        for triple in parser.by_ref() {
            let triple = triple.map_err(|e| refuse(format!("{path} is not valid Turtle: {e}")))?;
            if matches!(triple.subject, NamedOrBlankNode::BlankNode(_))
                || matches!(triple.object, Term::BlankNode(_))
            {
                return Err(refuse(format!(
                    "{path} has a blank node in `{triple}`; name every resource with an IRI (`<>` \
                     is the declaration itself)"
                )));
            }
            triples.push(triple);
        }
        // The one value of a property on `<>`, refusing two.
        let value = |local: &str| -> Result<Option<&Literal>> {
            let predicate = format!("{VOCABULARY_NAMESPACE}{local}");
            let mut found = triples.iter().filter(|t| {
                matches!(&t.subject, NamedOrBlankNode::NamedNode(s) if s.as_str() == base)
                    && t.predicate.as_str() == predicate
            });
            let first = found.next();
            if found.next().is_some() {
                return Err(refuse(format!(
                    "{path} gives `vector:{local}` more than once"
                )));
            }
            match first.map(|t| &t.object) {
                None => Ok(None),
                Some(Term::Literal(l)) => Ok(Some(l)),
                Some(other) => Err(refuse(format!(
                    "`vector:{local}` must be a literal, not {other}"
                ))),
            }
        };
        let word = |local: &str| -> Result<Option<String>> {
            match value(local)? {
                None => Ok(None),
                Some(l) if l.datatype().as_str() == XSD_STRING && !l.value().trim().is_empty() => {
                    Ok(Some(l.value().to_string()))
                }
                Some(l) => Err(refuse(format!(
                    "`vector:{local}` must be a non-empty string, not {l}"
                ))),
            }
        };
        let number = |local: &str, max: u64| -> Result<Option<u64>> {
            match value(local)? {
                None => Ok(None),
                Some(l) => Some(l)
                    .filter(|l| is_integer_datatype(l.datatype().as_str()))
                    .and_then(|l| l.value().parse::<u64>().ok())
                    .filter(|v| (1..=max).contains(v))
                    .map(Some)
                    .ok_or_else(|| {
                        refuse(format!(
                            "`vector:{local}` must be a whole number from 1 to {max}, not {l}"
                        ))
                    }),
            }
        };
        let model =
            word("model")?.ok_or_else(|| refuse(format!("{path} names no `vector:model`")))?;
        check_model(&model).map_err(|why| refuse(format!("`vector:model` {why}")))?;
        let dimensions = number("dimensions", 1 << 16)?
            .ok_or_else(|| refuse(format!("{path} names no `vector:dimensions`")))?;
        Ok(Declaration {
            index: n.to_string(),
            backend: word("backend")?.unwrap_or_else(|| FLAT.to_string()),
            model,
            provider: word("provider")?,
            dimensions: dimensions as usize,
            max_k: number("maxK", u64::from(MAX_K_LIMIT))?
                .map(|v| v as u32)
                .unwrap_or(DEFAULT_MAX_K),
            triples,
        })
    }

    /// Whether `other` builds the same vectors: the same backend, model, provider and
    /// dimensions. The `k` bound is not part of it.
    pub(crate) fn same_vectors(&self, other: &Declaration) -> bool {
        (&self.backend, &self.model, &self.provider, self.dimensions)
            == (
                &other.backend,
                &other.model,
                &other.provider,
                other.dimensions,
            )
    }

    fn to_text(&self) -> String {
        let provider = match &self.provider {
            Some(p) => format!("provider {p}"),
            None => "provider: the host's default".to_string(),
        };
        format!(
            "declaration of index {}\nbackend {}\nmodel {}\n{provider}\n{}\nk at most {}\n",
            self.index,
            self.backend,
            self.model,
            count(self.dimensions, "dimension", "dimensions"),
            self.max_k
        )
    }
}

/// Whether `datatype` is `xsd:integer` or one of the XSD types derived from it that can hold
/// a positive value: what a whole-number property of a declaration takes.
fn is_integer_datatype(datatype: &str) -> bool {
    datatype
        .strip_prefix("http://www.w3.org/2001/XMLSchema#")
        .is_some_and(|local| {
            matches!(
                local,
                "integer"
                    | "nonNegativeInteger"
                    | "positiveInteger"
                    | "long"
                    | "int"
                    | "short"
                    | "byte"
                    | "unsignedLong"
                    | "unsignedInt"
                    | "unsignedShort"
                    | "unsignedByte"
            )
        })
}

/// A model name: what an embedding's name carries in `{m}`.
///
/// The embedding template matches `{m}` up to the leftmost `:hash:`, so a name containing it,
/// or ending in `:hash` (which meets the template's own `:hash:`), would be read back as
/// another model.
pub(crate) fn check_model(m: &str) -> std::result::Result<(), String> {
    let ok = !m.is_empty()
        && m.len() <= 128
        && !m.contains(":hash:")
        && !m.ends_with(":hash")
        && m.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`{m}` is not a model name: use 1 to 128 letters, digits, `-`, `_`, `.`, `:` and `/`, \
             never containing `:hash:` or ending in `:hash`"
        ))
    }
}

/// Read and parse the declaration of index `n` through the kernel, under the caller's
/// capability. A miss hangs from the file's thread, so installing the declaration un-misses
/// it.
pub(crate) async fn read(inv: &Invocation<'_>, n: &str) -> Result<Declaration> {
    let file = declaration_file(n)?;
    let repr = match inv.source(&iri(&file)?).await {
        Ok(repr) => repr,
        Err(Error::NotFound(why)) => {
            inv.depends_on(file.as_str());
            return Err(Error::NotFound(format!(
                "index `{n}` is not declared: {why} (expected at vector/index/{n}.ttl in the \
                 config home, read as {file})"
            )));
        }
        Err(Error::Unresolved(_)) => {
            inv.depends_on(file.as_str());
            return Err(Error::NotFound(format!(
                "index `{n}` is not declared: nothing serves {file} in this kernel; a host serves \
                 declarations by binding a file module at {FILE_TEMPLATE} over the config home's \
                 vector directory"
            )));
        }
        Err(e) => return Err(e),
    };
    Declaration::parse(n, &repr.bytes)
}

/// The endpoint at [`DECLARATION_TEMPLATE`](crate::DECLARATION_TEMPLATE).
pub(crate) struct DeclarationEndpoint;

#[async_trait]
impl Endpoint for DeclarationEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let n = index_binding(inv)?;
        let face = face(inv, &[TEXT, TURTLE], "an index declaration")?;
        require(
            inv,
            &read_declaration_scope(n),
            &format!("reading the declaration of index `{n}`"),
        )?;
        let declaration = read(inv, n).await?;
        let bytes = if face == TEXT {
            declaration.to_text().into_bytes()
        } else {
            let mut triples = declaration.triples.clone();
            // Say what the module read, beside what the file said: its defaults included.
            let subject = named(&format!("{DECLARATION_PREFIX}{n}"))?;
            let typed = triples.iter().any(|t| {
                t.predicate.as_str() == RDF_TYPE
                    && matches!(&t.subject, NamedOrBlankNode::NamedNode(s) if *s == subject)
            });
            if !typed {
                triples.push(Triple::new(
                    subject.clone(),
                    named(RDF_TYPE)?,
                    term("IndexDeclaration"),
                ));
            }
            let has = |local: &str| {
                let p = term(local);
                triples.iter().any(|t| {
                    t.predicate == p
                        && matches!(&t.subject, NamedOrBlankNode::NamedNode(s) if *s == subject)
                })
            };
            let mut defaults = Vec::new();
            if !has("backend") {
                defaults.push(Triple::new(
                    subject.clone(),
                    term("backend"),
                    Literal::new_simple_literal(&declaration.backend),
                ));
            }
            if !has("maxK") {
                defaults.push(Triple::new(
                    subject.clone(),
                    term("maxK"),
                    Literal::from(i64::from(declaration.max_k)),
                ));
            }
            triples.extend(defaults);
            turtle(&triples, &format!("the declaration of index `{n}`"))?
        };
        Ok(Representation::new(ReprType::new(face), bytes).cacheable())
    }

    fn describe(&self) -> Description {
        Description::new(DECLARATION_ID)
            .title("Vector index declaration")
            .summary(
                "The declaration of index {n}: its backend, embedding model and provider, the \
                 dimensions the model is expected to produce, and the bound on k. Read through \
                 the kernel from urn:vector:file:path:index/{n}.ttl, which the host binds over the \
                 config home's vector directory, so the caller needs read authority there as well \
                 as urn:cap:vector:read:declaration:index:{n}. text/plain by default, \
                 as=text/turtle for the graph, with <> resolved to the declaration's own name and \
                 the defaults stated. A name with nothing behind it is not-found; a malformed \
                 declaration, or one with a blank node, is refused.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(READ_DECLARATION)
            .requires(FS_READ)
            .input(
                ArgSpec::new("n")
                    .summary("the index's name: the file vector/index/{n}.ttl in the config home")
                    .class(XSD_STRING)
                    .binding(),
            )
            .output(TEXT)
            .output(TURTLE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(model: &str, dimensions: &str) -> Result<Declaration> {
        Declaration::parse(
            "docs",
            format!(
                "@prefix vector: <https://ikigai-rs.dev/ns/vector#> .\n\
                 @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
                 <> vector:model \"{model}\" ; vector:dimensions {dimensions} .\n"
            )
            .as_bytes(),
        )
    }

    /// The embedding template matches `{m}` up to the leftmost `:hash:`, so a model ending in
    /// `:hash` would be read back as a shorter model and a hash beginning `hash:`: every
    /// member of such an index was refused, with a hash it never wrote.
    #[test]
    fn a_model_ending_in_hash_is_refused() {
        assert!(declared("x:hash", "4").is_err());
        assert_eq!(
            check_model("x:hash").unwrap_err(),
            "`x:hash` is not a model name: use 1 to 128 letters, digits, `-`, `_`, `.`, `:` and \
             `/`, never containing `:hash:` or ending in `:hash`"
        );
        assert!(check_model("hash:x").is_ok());
        assert!(check_model("x:hashed").is_ok());
        assert!(declared("embeddinggemma:300m", "4").is_ok());
    }

    /// A number is a typed integer literal: a string or a boolean that happens to spell
    /// digits is a malformed value, and refused.
    #[test]
    fn a_number_that_is_not_an_integer_literal_is_refused() {
        assert!(declared("m", "4").is_ok());
        assert!(declared("m", "\"4\"^^xsd:positiveInteger").is_ok());
        assert!(declared("m", "\"4\"").is_err());
        assert!(declared("m", "\"4\"^^xsd:boolean").is_err());
        assert!(declared("m", "\"4\"@en").is_err());
    }
}
