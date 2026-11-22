//! Every face and every sentence people read, pinned by its words (ACCESSIBILITY.md lists each
//! one with this file as its test). The text faces and the JSON face are pinned exactly; the
//! Turtle faces are pinned as the set of statements they parse to, so a serializer's layout is
//! free to change and its meaning is not.

mod common;

use std::collections::BTreeSet;

use common::*;
use ikigai_core::{Capability, Error, Verb};
use oxttl::TurtleParser;

fn host_with_two() -> Host {
    let host = Host::new();
    host.put_passage("p1", "vec:1,0,0,0");
    host.put_passage("p2", "vec:0.6,0.8,0,0");
    host
}

fn statements(turtle: &str) -> BTreeSet<String> {
    TurtleParser::new()
        .for_slice(turtle.as_bytes())
        .map(|t| t.expect("valid Turtle").to_string())
        .collect()
}

fn source(
    host: &Host,
    target: &str,
    face: Option<&str>,
    cap: &Capability,
) -> Result<String, Error> {
    let args: Vec<(&str, &str)> = face.map(|f| vec![("as", f)]).unwrap_or_default();
    host.issue(request(Verb::Source, target, &args), cap)
        .map(|r| text(&r))
}

const V: &str = "https://ikigai-rs.dev/ns/vector#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

#[test]
fn the_member_answers() {
    let host = host_with_two();
    let root = Capability::root();
    assert_eq!(
        host.add("a", "p1", &root).unwrap(),
        "added urn:example:passage:p1 to index a: 1 member\n"
    );
    assert_eq!(
        host.add("a", "p2", &root).unwrap(),
        "added urn:example:passage:p2 to index a: 2 members\n"
    );
    assert_eq!(
        host.add("a", "p2", &root).unwrap(),
        "refreshed urn:example:passage:p2 in index a: 2 members\n"
    );
    let exists = |id: &str| text(&host.issue(member(Verb::Exists, "a", id), &root).unwrap());
    assert_eq!(exists("p2"), "true");
    assert_eq!(
        host.remove("a", "p2", &root).unwrap(),
        "removed urn:example:passage:p2 from index a: 1 member\n"
    );
    assert_eq!(exists("p2"), "false");
    assert_eq!(
        host.remove("a", "p2", &root).unwrap(),
        "urn:example:passage:p2 is not a member of index a: 1 member\n"
    );
    assert_eq!(
        host.remove("b", "p2", &root).unwrap(),
        "urn:example:passage:p2 is not a member of index b: 0 members\n"
    );
    assert_eq!(
        host.rebuild("b", &root).unwrap(),
        "rebuilt index b: 0 members, model m no vectors\n"
    );
}

#[test]
fn the_nearest_faces() {
    let host = host_with_two();
    let root = Capability::root();
    host.add("a", "p1", &root).unwrap();
    host.add("a", "p2", &root).unwrap();
    let plain = [
        "2 passages, exact, model m digest sha256:aaaa1111",
        "  1. urn:example:passage:p1  score 1.000000",
        "  2. urn:example:passage:p2  score 0.600000",
        "",
    ]
    .join("\n");
    assert_eq!(
        host.nearest("a", "vec:1,0,0,0", "2", None, &root).unwrap(),
        plain
    );
    assert_eq!(
        host.nearest("a", "vec:1,0,0,0", "2", Some("text/plain"), &root)
            .unwrap(),
        plain
    );
    assert_eq!(
        host.nearest("a", "vec:1,0,0,0", "1", None, &root).unwrap(),
        "1 passage, exact, model m digest sha256:aaaa1111\n  1. urn:example:passage:p1  score 1.000000\n"
    );
    assert_eq!(
        host.nearest("a", "vec:1,0,0,0", "2", Some("application/json"), &root)
            .unwrap(),
        "{\"count\":2,\"digest\":\"sha256:aaaa1111\",\"exact\":true,\"hits\":[\
         {\"passage\":\"urn:example:passage:p1\",\"rank\":1,\"score\":1.0},\
         {\"passage\":\"urn:example:passage:p2\",\"rank\":2,\"score\":0.6}],\
         \"index\":\"a\",\"model\":\"m\"}"
    );
    let a = "<urn:vector:nearest:index:a>";
    let hit = |r: u32| format!("<urn:vector:nearest:index:a#rank-{r}>");
    let expected: BTreeSet<String> = [
        format!("{a} <{RDF_TYPE}> <{V}NearestAnswer>"),
        format!("{a} <{V}index> <urn:vector:index:name:a>"),
        format!("{a} <{V}exact> \"true\"^^<{XSD}boolean>"),
        format!("{a} <{V}model> \"m\""),
        format!("{a} <{V}hitCount> \"2\"^^<{XSD}integer>"),
        format!("{a} <{V}modelDigest> \"sha256:aaaa1111\""),
        format!("{a} <{V}hit> {}", hit(1)),
        format!("{a} <{V}hit> {}", hit(2)),
        format!("{} <{RDF_TYPE}> <{V}Hit>", hit(1)),
        format!("{} <{V}rank> \"1\"^^<{XSD}integer>", hit(1)),
        format!("{} <{V}passage> <urn:example:passage:p1>", hit(1)),
        format!("{} <{V}score> \"1.000000\"^^<{XSD}decimal>", hit(1)),
        format!("{} <{RDF_TYPE}> <{V}Hit>", hit(2)),
        format!("{} <{V}rank> \"2\"^^<{XSD}integer>", hit(2)),
        format!("{} <{V}passage> <urn:example:passage:p2>", hit(2)),
        format!("{} <{V}score> \"0.600000\"^^<{XSD}decimal>", hit(2)),
    ]
    .into_iter()
    .collect();
    let turtle = host
        .nearest("a", "vec:1,0,0,0", "2", Some("text/turtle"), &root)
        .unwrap();
    assert_eq!(statements(&turtle), expected, "{turtle}");
}

#[test]
fn an_empty_index_answers_no_passages() {
    let host = Host::new();
    let root = Capability::root();
    host.rebuild("a", &root).unwrap();
    assert_eq!(
        host.nearest("a", "anything", "3", None, &root).unwrap(),
        "0 passages, exact, model m: the index holds no vectors\n"
    );
    assert_eq!(
        source(&host, "urn:vector:index:name:a", None, &root).unwrap(),
        [
            "index a",
            "backend flat",
            "exact",
            "model m",
            "no model digest yet: the index holds no vectors",
            "4 dimensions",
            "0 members",
            // The SHA-256 of nothing.
            "member list sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "k at most 5",
            "",
        ]
        .join("\n")
    );
}

#[test]
fn the_index_and_declaration_faces() {
    let host = host_with_two();
    let root = Capability::root();
    host.add("a", "p1", &root).unwrap();
    host.add("a", "p2", &root).unwrap();
    assert_eq!(
        source(&host, "urn:vector:index:name:a", None, &root).unwrap(),
        [
            "index a",
            "backend flat",
            "exact",
            "model m",
            "model digest sha256:aaaa1111",
            "4 dimensions",
            "2 members",
            "member list sha256:710028d74100a477e9724af77428b9400d72c37e424c68986699111c80bf5709",
            "k at most 5",
            "",
        ]
        .join("\n")
    );
    let i = "<urn:vector:index:name:a>";
    let expected: BTreeSet<String> = [
        format!("{i} <{RDF_TYPE}> <{V}Index>"),
        format!("{i} <{V}declaration> <urn:vector:declaration:index:a>"),
        format!("{i} <{V}backend> \"flat\""),
        format!("{i} <{V}exact> \"true\"^^<{XSD}boolean>"),
        format!("{i} <{V}model> \"m\""),
        format!("{i} <{V}dimensions> \"4\"^^<{XSD}integer>"),
        format!("{i} <{V}memberCount> \"2\"^^<{XSD}integer>"),
        format!(
            "{i} <{V}memberListDigest> \
             \"sha256:710028d74100a477e9724af77428b9400d72c37e424c68986699111c80bf5709\""
        ),
        format!("{i} <{V}maxK> \"5\"^^<{XSD}integer>"),
        format!("{i} <{V}modelDigest> \"sha256:aaaa1111\""),
    ]
    .into_iter()
    .collect();
    let turtle = source(&host, "urn:vector:index:name:a", Some("text/turtle"), &root).unwrap();
    assert_eq!(statements(&turtle), expected, "{turtle}");

    assert_eq!(
        source(&host, "urn:vector:declaration:index:b", None, &root).unwrap(),
        "declaration of index b\nbackend flat\nmodel m\nprovider: the host's default\n4 \
         dimensions\nk at most 50\n"
    );
    host.declare(
        "c",
        "@prefix vector: <https://ikigai-rs.dev/ns/vector#> .\n<> vector:model \"m\" ; \
         vector:provider \"ollama\" ; vector:dimensions 1 .",
    );
    assert_eq!(
        source(&host, "urn:vector:declaration:index:c", None, &root).unwrap(),
        "declaration of index c\nbackend flat\nmodel m\nprovider ollama\n1 dimension\nk at most \
         50\n"
    );
    let d = "<urn:vector:declaration:index:c>";
    let expected: BTreeSet<String> = [
        format!("{d} <{RDF_TYPE}> <{V}IndexDeclaration>"),
        format!("{d} <{V}backend> \"flat\""),
        format!("{d} <{V}model> \"m\""),
        format!("{d} <{V}provider> \"ollama\""),
        format!("{d} <{V}dimensions> \"1\"^^<{XSD}integer>"),
        format!("{d} <{V}maxK> \"50\"^^<{XSD}integer>"),
    ]
    .into_iter()
    .collect();
    let turtle = source(
        &host,
        "urn:vector:declaration:index:c",
        Some("text/turtle"),
        &root,
    )
    .unwrap();
    assert_eq!(statements(&turtle), expected, "{turtle}");
}

#[test]
fn the_embedding_face() {
    let host = Host::new();
    let name = ikigai_vector::embedding_name("m", "vec:3,4,0,0");
    let answer = host
        .issue(
            request(Verb::Source, &name, &[("text", "vec:3,4,0,0")]),
            &Capability::root(),
        )
        .unwrap();
    assert_eq!(answer.repr_type.media_type, "application/json");
    assert_eq!(
        text(&answer),
        "{\"digest\":\"sha256:aaaa1111\",\"dimensions\":4,\"model\":\"m:latest\",\"vector\":[3.0,4.0,0.0,0.0]}"
    );
}

#[test]
fn every_refusal_sentence() {
    let host = host_with_two();
    let root = Capability::root();
    // Not built yet.
    let not_built = "not found: index `a` is not built: add a member with `sink \
                     urn:vector:member:index:a:passage:{p}`, or build it empty with `sink \
                     urn:vector:index:name:a`";
    assert_eq!(
        host.nearest("a", "q", "1", None, &root)
            .unwrap_err()
            .to_string(),
        not_built
    );
    assert_eq!(
        source(&host, "urn:vector:index:name:a", None, &root)
            .unwrap_err()
            .to_string(),
        not_built
    );
    // Not declared.
    assert_eq!(
        host.add("zzz", "p1", &root).unwrap_err().to_string(),
        "not found: index `zzz` is not declared: no file index/zzz.ttl (expected at \
         vector/index/zzz.ttl in the config home, read as urn:vector:file:path:index/zzz.ttl)"
    );
    // A passage that is not there.
    assert_eq!(
        host.add("a", "nope", &root).unwrap_err().to_string(),
        "not found: the passage urn:example:passage:nope is not there to add to index `a`: no \
         passage nope"
    );
    host.add("a", "p1", &root).unwrap();
    // k out of range, naming k and the bound.
    assert_eq!(
        host.nearest("a", "q", "9", None, &root)
            .unwrap_err()
            .to_string(),
        "invalid argument `k`: k must be a whole number from 1 to 5, the bound the declaration \
         of index `a` sets"
    );
    assert_eq!(
        host.nearest("a", "  ", "1", None, &root)
            .unwrap_err()
            .to_string(),
        "invalid argument `text`: `text` is empty, so there is nothing to embed"
    );
    // A digest mismatch, naming both.
    host.set_digest(OTHER_DIGEST);
    assert_eq!(
        host.nearest("a", "q", "1", None, &root)
            .unwrap_err()
            .to_string(),
        "conflict: index `a` holds vectors from model m digest sha256:aaaa1111, and the \
         embedding came from digest sha256:bbbb2222; vectors from two digests are never \
         compared, so rebuild it with `sink urn:vector:index:name:a`"
    );
    // A declaration that no longer builds the vectors the index holds.
    host.declare(
        "a",
        "@prefix vector: <https://ikigai-rs.dev/ns/vector#> .\n<> vector:model \"m2\" ; \
         vector:dimensions 4 .",
    );
    assert_eq!(
        host.add("a", "p2", &root).unwrap_err().to_string(),
        "conflict: the declaration of index `a` now builds flat vectors of model m2 with 4 \
         dimensions, and the index holds flat vectors of model m with 4 dimensions; rebuild it \
         with `sink urn:vector:index:name:a`"
    );
    // A model that answers other dimensions than declared.
    host.declare(
        "d",
        "@prefix vector: <https://ikigai-rs.dev/ns/vector#> .\n<> vector:model \"m\" ; \
         vector:dimensions 3 .",
    );
    assert_eq!(
        host.add("d", "p1", &root).unwrap_err().to_string(),
        "conflict: model m answered 4 dimensions, and the declaration of index `d` says 3 \
         dimensions; fix the declaration or the model"
    );
    // Malformed declarations.
    host.declare("e", "<> <https://ikigai-rs.dev/ns/vector#dimensions> 4 .");
    assert_eq!(
        host.add("e", "p1", &root).unwrap_err().to_string(),
        "endpoint error: the declaration of index `e`: vector/index/e.ttl names no `vector:model`"
    );
    host.declare("f", "<> <https://ikigai-rs.dev/ns/vector#model> [] .");
    assert!(host
        .add("f", "p1", &root)
        .unwrap_err()
        .to_string()
        .contains("has a blank node"));
    // A face nobody serves.
    assert_eq!(
        host.nearest("a", "q", "1", Some("text/html"), &root)
            .unwrap_err()
            .to_string(),
        "invalid argument `as`: `text/html` is not a face of a nearest answer: ask for \
         text/plain, text/turtle, application/json (the first is the default)"
    );
    // A bad index name, and a bad passage name.
    assert_eq!(
        source(&host, "urn:vector:index:name:..", None, &root)
            .unwrap_err()
            .to_string(),
        "invalid argument `n`: `..` is not an index name: use 1 to 64 letters, digits, `-`, `_` \
         and `.`, not starting with `.` (the file vector/index/{n}.ttl in the config home)"
    );
    // An embedding asked for by its hash alone.
    let name = ikigai_vector::embedding_name("m", "anything");
    assert_eq!(
        source(&host, &name, None, &root).unwrap_err().to_string(),
        "not found: the embedding of hash \
         ee0874170b7f6f32b8c6aa8b8c8a6b4b52b8b5a4ec6a8a7b0b8bb3b8e5b4e0b0 by model m is not \
         cached here, and a hash alone cannot be embedded: an embedding is made when its passage \
         is added to an index, `sink urn:vector:member:index:{n}:passage:{p}`, which holds the \
         text"
            .replace(
                "ee0874170b7f6f32b8c6aa8b8c8a6b4b52b8b5a4ec6a8a7b0b8bb3b8e5b4e0b0",
                name.rsplit(':').next().unwrap()
            )
    );
    assert_eq!(
        host.issue(
            request(Verb::Source, &name, &[("text", "something else")]),
            &root
        )
        .unwrap_err()
        .to_string(),
        format!(
            "invalid argument `text`: the SHA-256 of `text` is not the hash {} this name carries",
            name.rsplit(':').next().unwrap()
        )
    );
}
