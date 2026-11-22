//! The module's own vocabulary, `src/vocabulary.ttl`: every term the crate emits is defined
//! there with a label and a comment, and the ontology carries the crate's version.

use std::collections::BTreeSet;

use ikigai_vector::{VOCABULARY, VOCABULARY_NAMESPACE};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use oxttl::TurtleParser;

/// Every Rust source of the crate, compiled in so the test opens no file. A new module adds
/// itself here.
const SOURCES: &[&str] = &[
    include_str!("../src/lib.rs"),
    include_str!("../src/backend.rs"),
    include_str!("../src/declaration.rs"),
    include_str!("../src/embedding.rs"),
    include_str!("../src/index.rs"),
    include_str!("../src/nearest.rs"),
];

fn vocabulary() -> Vec<Triple> {
    TurtleParser::new()
        .for_slice(VOCABULARY.as_bytes())
        .collect::<Result<_, _>>()
        .expect("the vocabulary is well-formed Turtle")
}

fn defined(triples: &[Triple]) -> BTreeSet<String> {
    triples
        .iter()
        .filter_map(|t| match &t.subject {
            NamedOrBlankNode::NamedNode(s) if s.as_str().starts_with(VOCABULARY_NAMESPACE) => {
                Some(s.as_str()[VOCABULARY_NAMESPACE.len()..].to_string())
            }
            _ => None,
        })
        .collect()
}

/// The local names the sources pass to `term("…")`, the one way the crate spells a term.
fn used() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for source in SOURCES {
        for (at, _) in source.match_indices("term(\"") {
            let rest = &source[at + 6..];
            let end = rest.find('"').expect("a closing quote");
            out.insert(rest[..end].to_string());
        }
    }
    out
}

#[test]
fn every_term_the_crate_uses_is_defined() {
    let defined = defined(&vocabulary());
    let used = used();
    assert!(used.len() >= 15, "{used:?}");
    let missing: Vec<&String> = used.difference(&defined).collect();
    assert!(missing.is_empty(), "used but not defined: {missing:?}");
    // The declaration's own terms, read by name rather than through `term`.
    for local in ["backend", "model", "provider", "dimensions", "maxK"] {
        assert!(defined.contains(local), "{local}");
    }
}

#[test]
fn every_term_has_a_label_and_a_comment() {
    let triples = vocabulary();
    for local in defined(&triples) {
        let iri = format!("{VOCABULARY_NAMESPACE}{local}");
        for predicate in [
            "http://www.w3.org/2000/01/rdf-schema#label",
            "http://www.w3.org/2000/01/rdf-schema#comment",
            "http://www.w3.org/2000/01/rdf-schema#isDefinedBy",
        ] {
            assert!(
                triples.iter().any(
                    |t| matches!(&t.subject, NamedOrBlankNode::NamedNode(s) if s.as_str() == iri)
                        && t.predicate.as_str() == predicate
                ),
                "{local} has no {predicate}"
            );
        }
    }
}

#[test]
fn the_ontology_carries_the_crate_version() {
    let ontology = VOCABULARY_NAMESPACE.trim_end_matches('#');
    let version: Vec<String> = vocabulary()
        .into_iter()
        .filter(|t| {
            matches!(&t.subject, NamedOrBlankNode::NamedNode(s) if s.as_str() == ontology)
                && t.predicate.as_str() == "http://www.w3.org/2002/07/owl#versionInfo"
        })
        .filter_map(|t| match t.object {
            Term::Literal(l) => Some(l.value().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(version, [env!("CARGO_PKG_VERSION")]);
}
