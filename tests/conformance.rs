//! The module recipe as one test: `ikigai-conformance` walks every endpoint the vector space
//! binds and reports every violation at once, with no check skipped.
//!
//! - The kernel is the one a host builds: the module's space, and beside it the three things a
//!   host binds (the file mount, the passages, `urn:llm:embed`), stubbed in `tests/common`.
//! - The index is built before the walk, so nearest, the description and `exists` have
//!   something to answer. The member `sink` fixture re-adds a member, the `delete` fixture
//!   removes a passage that is not one (a delete is idempotent), and the rebuild keeps every
//!   member: the walk changes nothing another fixture reads.
//! - `cacheable`: every read is marked cacheable and hangs from a thread a change cuts.
//! - `enforced_under`: a caller holding the grant for ANOTHER index is refused by the module's
//!   own rule, with a typed `Denied`.
//! - The space is host-named: it serves the state it was handed.

mod common;

use common::*;
use ikigai_conformance::{Expected, Fixture, Suite};
use ikigai_core::{Capability, Verb};

const OWN: [&str; 6] = [
    ikigai_vector::DECLARATION_ID,
    ikigai_vector::INDEX_ID,
    ikigai_vector::MEMBER_ID,
    ikigai_vector::EMBEDDING_ID,
    ikigai_vector::NEAREST_ID,
    ikigai_vector::CHANGE_ID,
];

fn with_fs(scope: String) -> Capability {
    Capability::scoped([scope, FS.to_string()])
}

#[test]
fn conforms() {
    let host = Host::new();
    host.put_passage("p1", "vec:1,0,0,0");
    host.put_passage("p2", "vec:0.6,0.8,0,0");
    host.add("a", "p1", &writer("a")).unwrap();
    host.add("a", "p2", &writer("a")).unwrap();

    let index = |verb: Verb| Fixture::new(ikigai_vector::INDEX_ID, verb).binding("n", "a");
    let member = |verb: Verb, p: &str| {
        Fixture::new(ikigai_vector::MEMBER_ID, verb)
            .binding("n", "a")
            .binding("p", passage(p))
    };
    let nearest = || {
        Fixture::new(ikigai_vector::NEAREST_ID, Verb::Source)
            .binding("n", "a")
            .arg("text", "vec:1,0,0,0")
            .arg("k", "2")
    };
    let declaration =
        || Fixture::new(ikigai_vector::DECLARATION_ID, Verb::Source).binding("n", "a");
    let change = || Fixture::new(ikigai_vector::CHANGE_ID, Verb::Sink).binding("n", "a");
    let text = "vec:1,0,0,0";
    let embedding = || {
        let name = ikigai_vector::embedding_name("m", text);
        let hash = name.rsplit(':').next().unwrap().to_string();
        Fixture::new(ikigai_vector::EMBEDDING_ID, Verb::Source)
            .binding("m", "m")
            .binding("h", hash)
            .arg("text", text)
    };

    let report = Suite::new()
        .body_space(host.vectors.space())
        .own_all(OWN)
        .inherit(
            "file",
            "the file mount a host binds at urn:vector:file:path:{path} over the config home's \
             vector directory (ikigai-fs); a stub here, and ikigai-fs's own conformance test \
             covers the real one",
        )
        .inherit(
            "passage",
            "the passages an index holds, served by another module (ikigai-knowledge's); a stub \
             here",
        )
        .inherit(
            "embed-stub",
            "the model, ikigai-llm's urn:llm:embed, bound beside the module by the host; a stub \
             here, and ikigai-llm's own conformance test covers the real one",
        )
        .host_named_space("Vectors::space()", host.vectors.space())
        .vocabulary(ikigai_vector::VOCABULARY)
        .cacheable(ikigai_vector::DECLARATION_ID)
        .cacheable(ikigai_vector::INDEX_ID)
        .cacheable(ikigai_vector::MEMBER_ID)
        .cacheable(ikigai_vector::EMBEDDING_ID)
        .cacheable(ikigai_vector::NEAREST_ID)
        .fixture(declaration())
        .fixture(index(Verb::Source))
        .fixture(index(Verb::Sink))
        .fixture(member(Verb::Sink, "p1"))
        .fixture(member(Verb::Delete, "gone"))
        .fixture(member(Verb::Exists, "p1"))
        .fixture(embedding())
        .fixture(nearest())
        .fixture(change())
        .enforced_under(
            declaration().capability(with_fs(ikigai_vector::read_declaration_scope("b"))),
        )
        .enforced_under(index(Verb::Source).capability(reader("b")))
        .enforced_under(index(Verb::Sink).capability(writer("b")))
        .enforced_under(member(Verb::Sink, "p1").capability(writer("b")))
        .enforced_under(member(Verb::Delete, "p1").capability(writer("b")))
        .enforced_under(member(Verb::Exists, "p1").capability(reader("b")))
        .enforced_under(nearest().capability(reader("b")))
        .enforced_under(change().capability(writer("b")))
        .run_blocking(&host.kernel);
    eprintln!("{report}");
    report.assert_clean_with(&Expected::new().walked(OWN));
    assert_eq!(
        report
            .checks
            .skipped()
            .map(|c| c.to_string())
            .collect::<Vec<_>>(),
        Vec::<String>::new(),
        "every check runs: {report}"
    );
    // The walk left the index as it found it.
    assert_eq!(host.vectors.held("a").unwrap().len(), 2);
}
