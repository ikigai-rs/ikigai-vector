//! Authority under the NARROW published grants (README, Capabilities): each test caller holds
//! exactly the tokens the README lists, never root, so an over-declared floor or a missing
//! module rule fails here rather than in a host.

mod common;

use common::*;
use ikigai_core::{Capability, Error, Verb};

fn denied(result: Result<String, Error>) -> String {
    match result {
        Err(Error::Denied(why)) => why.message().to_string(),
        other => panic!("expected a typed Denied, got {other:?}"),
    }
}

fn built() -> Host {
    let host = Host::new();
    host.put_passage("p1", "vec:1,0,0,0");
    host.add("a", "p1", &writer("a")).unwrap();
    host.add("b", "p1", &writer("b")).unwrap();
    host
}

#[test]
fn the_published_grants_are_enough() {
    let host = built();
    let r = reader("a");
    assert!(host.nearest("a", "vec:1,0,0,0", "1", None, &r).is_ok());
    assert!(host
        .issue(request(Verb::Source, "urn:vector:index:name:a", &[]), &r)
        .is_ok());
    assert_eq!(
        text(&host.issue(member(Verb::Exists, "a", "p1"), &r).unwrap()),
        "true"
    );
    let w = writer("a");
    assert!(host.remove("a", "p1", &w).is_ok());
    assert!(host.add("a", "p1", &w).is_ok());
    assert!(host.rebuild("a", &w).is_ok());
    let declaration_reader =
        Capability::scoped([ikigai_vector::read_declaration_scope("a"), FS.to_string()]);
    assert!(host
        .issue(
            request(Verb::Source, "urn:vector:declaration:index:a", &[]),
            &declaration_reader
        )
        .is_ok());
}

#[test]
fn a_reader_of_one_index_cannot_read_another() {
    let host = built();
    let r = reader("a");
    assert_eq!(
        denied(host.nearest("b", "vec:1,0,0,0", "1", None, &r)),
        "reading index `b` takes `urn:cap:vector:read:index:name:b`, which this capability does \
         not hold"
    );
    denied(
        host.issue(request(Verb::Source, "urn:vector:index:name:b", &[]), &r)
            .map(|r| text(&r)),
    );
    denied(
        host.issue(member(Verb::Exists, "b", "p1"), &r)
            .map(|r| text(&r)),
    );
}

#[test]
fn a_reader_cannot_write() {
    let host = built();
    let r = reader("a");
    denied(host.add("a", "p1", &r));
    denied(host.remove("a", "p1", &r));
    denied(host.rebuild("a", &r));
    denied(
        host.issue(request(Verb::Sink, "urn:vector:change:index:a", &[]), &r)
            .map(|r| text(&r)),
    );
    // And the index still holds its member.
    assert_eq!(host.vectors.held("a").unwrap().len(), 1);
}

#[test]
fn a_write_without_the_token_is_denied() {
    let host = built();
    // A writer of `b` holds a write grant, so the kernel's floor admits the call; the module's
    // own rule refuses it by name.
    assert_eq!(
        denied(host.add("a", "p1", &writer("b"))),
        "adding a member to index `a` takes `urn:cap:vector:write:index:name:a`, which this \
         capability does not hold"
    );
    assert_eq!(
        denied(host.remove("a", "p1", &writer("b"))),
        "removing a member from index `a` takes `urn:cap:vector:write:index:name:a`, which this \
         capability does not hold"
    );
    assert_eq!(
        denied(host.rebuild("a", &writer("b"))),
        "rebuilding index `a` takes `urn:cap:vector:write:index:name:a`, which this capability \
         does not hold"
    );
    // A caller holding nothing is refused by the kernel's floor.
    let nothing = Capability::scoped(Vec::<String>::new());
    denied(host.add("a", "p1", &nothing));
    denied(host.rebuild("a", &nothing));
    // A writer cannot read nearest: writing is not reading.
    denied(host.nearest("a", "vec:1,0,0,0", "1", None, &writer("a")));
    assert_eq!(host.vectors.held("a").unwrap().len(), 1);
}

#[test]
fn the_declaration_takes_its_own_token() {
    let host = built();
    // The index reader does not read the declaration.
    let r = Capability::scoped([ikigai_vector::read_index_scope("a"), FS.to_string()]);
    denied(
        host.issue(
            request(Verb::Source, "urn:vector:declaration:index:a", &[]),
            &r,
        )
        .map(|r| text(&r)),
    );
    let other = Capability::scoped([ikigai_vector::read_declaration_scope("b"), FS.to_string()]);
    assert_eq!(
        denied(
            host.issue(
                request(Verb::Source, "urn:vector:declaration:index:a", &[]),
                &other,
            )
            .map(|r| text(&r))
        ),
        "reading the declaration of index `a` takes `urn:cap:vector:read:declaration:index:a`, \
         which this capability does not hold"
    );
}
