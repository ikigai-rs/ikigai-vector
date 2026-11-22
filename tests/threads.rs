//! Cacheability (design, section 3): nearest, the index description and a member's `exists`
//! are cached and hang from the index's thread, which every member change and rebuild cuts.
//! The embedding is cached on the text and the model, so a repeated query embeds once.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use ikigai_core::Verb;

fn nearest_request(query: &str) -> ikigai_core::Request {
    request(
        Verb::Source,
        "urn:vector:nearest:index:a",
        &[("text", query), ("k", "5")],
    )
}

#[test]
fn nearest_is_cached_until_a_member_changes() {
    let host = Host::new();
    let (w, r) = (writer("a"), reader("a"));
    host.put_passage("p1", "vec:1,0,0,0");
    host.put_passage("p2", "vec:0,1,0,0");
    host.add("a", "p1", &w).unwrap();
    let first = host.issue(nearest_request("vec:1,0,0,0"), &r).unwrap();
    assert!(host.kernel.is_cached(&nearest_request("vec:1,0,0,0"), &r));
    let describe = request(Verb::Source, "urn:vector:index:name:a", &[]);
    host.issue(describe.clone(), &r).unwrap();
    assert!(host.kernel.is_cached(&describe, &r));
    let exists = member(Verb::Exists, "a", "p2");
    assert_eq!(text(&host.issue(exists.clone(), &r).unwrap()), "false");
    assert!(host.kernel.is_cached(&exists, &r));

    // Adding a member cuts all three.
    host.add("a", "p2", &w).unwrap();
    assert!(!host.kernel.is_cached(&nearest_request("vec:1,0,0,0"), &r));
    assert!(!host.kernel.is_cached(&describe, &r));
    assert!(!host.kernel.is_cached(&exists, &r));
    let second = host.issue(nearest_request("vec:1,0,0,0"), &r).unwrap();
    assert_ne!(first.bytes, second.bytes);
    assert!(text(&second).starts_with("2 passages"));
    assert_eq!(text(&host.issue(exists.clone(), &r).unwrap()), "true");

    // So does removing one, and so does a rebuild.
    host.remove("a", "p2", &w).unwrap();
    assert!(!host.kernel.is_cached(&nearest_request("vec:1,0,0,0"), &r));
    host.issue(nearest_request("vec:1,0,0,0"), &r).unwrap();
    host.rebuild("a", &w).unwrap();
    assert!(!host.kernel.is_cached(&nearest_request("vec:1,0,0,0"), &r));
    assert_eq!(text(&host.issue(exists, &r).unwrap()), "false");
}

#[test]
fn a_change_to_one_index_leaves_another_cached() {
    let host = Host::new();
    host.put_passage("p1", "vec:1,0,0,0");
    host.add("a", "p1", &writer("a")).unwrap();
    host.add("b", "p1", &writer("b")).unwrap();
    host.issue(nearest_request("vec:1,0,0,0"), &reader("a"))
        .unwrap();
    host.put_passage("p2", "vec:0,1,0,0");
    host.add("b", "p2", &writer("b")).unwrap();
    assert!(host
        .kernel
        .is_cached(&nearest_request("vec:1,0,0,0"), &reader("a")));
}

#[test]
fn a_repeated_query_is_embedded_once() {
    let host = Host::new();
    let (w, r) = (writer("a"), reader("a"));
    host.put_passage("p1", "vec:1,0,0,0");
    host.add("a", "p1", &w).unwrap();
    let before = host.embeds.load(Ordering::SeqCst);
    host.issue(nearest_request("a question"), &r).unwrap();
    host.put_passage("p2", "vec:0,1,0,0");
    host.add("a", "p2", &w).unwrap();
    // Nearest recomputes after the change, and its query's embedding comes from the cache.
    let after_add = host.embeds.load(Ordering::SeqCst);
    host.issue(nearest_request("a question"), &r).unwrap();
    assert_eq!(host.embeds.load(Ordering::SeqCst), after_add);
    // One for the query, one for p2.
    assert_eq!(after_add - before, 2);
    // A rebuild at the same digest re-embeds nothing either: every embedding is cached.
    host.rebuild("a", &w).unwrap();
    assert_eq!(host.embeds.load(Ordering::SeqCst), after_add);
}
