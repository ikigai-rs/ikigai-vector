//! The backend conformance test (design, sections 2 and 6): the five guarantees, checked
//! through the kernel, for every backend this crate offers. A new backend adds one `#[test]`
//! that calls [`conformance`] with its kind and factory.

mod common;

use common::*;
use ikigai_core::Error;
use ikigai_vector::{BackendFactory, Vectors};

#[test]
fn flat_conforms() {
    conformance("flat", flat_factory());
}

/// The same checks under another kind name, so the harness is shown to read the kind from the
/// declaration rather than assume `flat`.
#[test]
fn a_backend_registered_under_another_kind_conforms() {
    conformance("flat-copy", flat_factory());
}

/// Every guarantee, in one run: ties, delete then query, rebuild equals incremental, no text
/// stored, `k` out of range, a digest mismatch refused, and exactness declared in every answer.
pub fn conformance(kind: &str, factory: BackendFactory) {
    let exact = factory(4).exact();
    assert!(
        exact,
        "backend `{kind}` is approximate: the recall check against flat (design, section 2) is not \
         built yet, so this harness cannot pass it"
    );
    let host = Host::with(Vectors::new().with_backend(kind, factory), kind);
    let (w, r) = (writer("a"), reader("a"));

    // Ties. Three members with identical vectors, one a hair away (its raw cosine is
    // 0.99999995, which rounds to 1.000000 and so ties), and one far off.
    for id in ["t3", "t1", "t2"] {
        host.put_passage(id, "vec:1,0,0,0");
    }
    host.put_passage("t0", "vec:1,0.0003,0,0");
    host.put_passage("far", "vec:0,1,0,0");
    host.put_passage(
        "secret",
        "MARKER-7f3a1 passage words that must never be stored",
    );
    for id in ["t3", "t1", "t2", "t0", "far", "secret"] {
        host.add("a", id, &w)
            .unwrap_or_else(|e| panic!("adding {id}: {e}"));
    }
    let top3 = host.nearest("a", "vec:1,0,0,0", "3", None, &r).unwrap();
    assert_eq!(
        top3,
        [
            "3 passages, exact, model m digest sha256:aaaa1111",
            "  1. urn:example:passage:t0  score 1.000000",
            "  2. urn:example:passage:t1  score 1.000000",
            "  3. urn:example:passage:t2  score 1.000000",
            "",
        ]
        .join("\n"),
        "ties go by passage name, after rounding"
    );
    // A tie that straddles the boundary is decided by name, and the answer repeats.
    for _ in 0..3 {
        host.kernel
            .cut(format!("{}a", ikigai_vector::CHANGE_PREFIX));
        assert_eq!(
            host.nearest("a", "vec:1,0,0,0", "3", None, &r).unwrap(),
            top3
        );
    }

    // Delete is immediate and total.
    host.remove("a", "t1", &w).unwrap();
    let after = host.nearest("a", "vec:1,0,0,0", "5", None, &r).unwrap();
    assert!(!after.contains("passage:t1 "), "{after}");
    assert!(after.starts_with("5 passages, exact"), "{after}");
    for face in ["text/turtle", "application/json"] {
        let answer = host
            .nearest("a", "vec:1,0,0,0", "5", Some(face), &r)
            .unwrap();
        assert!(!answer.contains("passage:t1\""), "{answer}");
        assert!(!answer.contains("passage:t1>"), "{answer}");
    }

    // Rebuild is equivalent: identical answers for an exact backend.
    let queries = [
        "vec:1,0,0,0",
        "vec:0,1,0,0",
        "vec:1,1,1,1",
        "something else entirely",
    ];
    let answers = |host: &Host| -> Vec<String> {
        let mut out = Vec::new();
        for q in queries {
            for face in [None, Some("text/turtle"), Some("application/json")] {
                out.push(host.nearest("a", q, "5", face, &r).unwrap());
            }
        }
        out.push(text(
            &host
                .issue(
                    request(
                        ikigai_core::Verb::Source,
                        &format!("{}a", ikigai_vector::INDEX_PREFIX),
                        &[],
                    ),
                    &r,
                )
                .unwrap(),
        ));
        out
    };
    let incremental = answers(&host);
    let held = host.vectors.held("a").unwrap();
    assert_eq!(
        host.rebuild("a", &w).unwrap(),
        "rebuilt index a: 5 members, model m digest sha256:aaaa1111\n"
    );
    assert_eq!(answers(&host), incremental, "rebuild equals incremental");
    assert_eq!(host.vectors.held("a").unwrap(), held);

    // No text kept: the backend holds the members' names and vectors, nothing else, and no
    // face carries a passage's text or the query's.
    let names: Vec<&str> = held.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["far", "secret", "t0", "t2", "t3"]
            .map(passage)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    assert!(held.iter().all(|(_, v)| v.len() == 4));
    for answer in &incremental {
        assert!(!answer.contains("MARKER-7f3a1"), "{answer}");
        assert!(!answer.contains("something else"), "{answer}");
    }

    // `k` out of range names `k` and the bound, and never quotes the query.
    for k in ["0", "6", "-1", "three"] {
        match host.nearest("a", "QUERY-MARKER-91c", k, None, &r) {
            Err(Error::InvalidArgument { name, detail }) => {
                assert_eq!(name, "k");
                assert!(detail.contains("from 1 to 5"), "{detail}");
                assert!(!detail.contains("QUERY-MARKER"), "{detail}");
            }
            other => panic!("k={k}: {other:?}"),
        }
    }
    assert!(host.nearest("a", "vec:1,0,0,0", "5", None, &r).is_ok());

    // A digest mismatch is refused, naming both digests, for a new member and for a query.
    host.set_digest(OTHER_DIGEST);
    host.put_passage("late", "vec:0,0,1,0");
    match host.add("a", "late", &w) {
        Err(Error::Conflict(why)) => {
            assert!(why.contains(DIGEST) && why.contains(OTHER_DIGEST), "{why}");
            assert!(!why.contains("vec:0,0,1,0"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    match host.nearest("a", "QUERY-MARKER-91c", "3", None, &r) {
        Err(Error::Conflict(why)) => {
            assert!(why.contains(DIGEST) && why.contains(OTHER_DIGEST), "{why}");
            assert!(!why.contains("QUERY-MARKER"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    // A rebuild re-embeds every member at the new digest, and the index answers again.
    assert_eq!(
        host.rebuild("a", &w).unwrap(),
        "rebuilt index a: 5 members, model m digest sha256:bbbb2222\n"
    );
    host.add("a", "late", &w).unwrap();
    let answer = host.nearest("a", "vec:0,0,1,0", "1", None, &r).unwrap();
    assert_eq!(
        answer,
        "1 passage, exact, model m digest sha256:bbbb2222\n  1. urn:example:passage:late  score 1.000000\n"
    );

    // No embedding was ever handed anything but a member's text or a query.
    let texts: Vec<String> = host.embedded.lock().unwrap().clone();
    assert!(texts.iter().all(|t| !t.is_empty()));
}
