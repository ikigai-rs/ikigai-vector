# ikigai-vector

Vector search as [ikigai](https://github.com/ikigai-rs) resources: one contract, pluggable
backends. Given a question's text, it answers the **names** of the nearest passages, never their
text, so reading what a name holds stays under the caller's own capability.

**A scaffold.** Nothing below is built yet; ledger #3890 builds it, against the design in
[docs/design.md](docs/design.md), which this README follows.

## Resource map

| Set | Name | Kind | Verb |
|---|---|---|---|
| index declaration | `urn:vector:declaration:index:{n}` | ATOM (config) | `source` |
| index | `urn:vector:index:name:{n}` | ATOM (a materialized cache) | `source`, `sink` (rebuild) |
| member | `urn:vector:member:index:{n}:passage:{p}` | Sink edge | `sink`, `delete`, `exists` |
| embedding | `urn:vector:embedding:model:{m}:hash:{h}` | composition | `source` |
| nearest | `urn:vector:nearest:index:{n}` (`text=`, `k=`) | composition | `source` |

Every name is planned. A pull request that builds one marks it built here.

## Capabilities

Per index, never spanning indexes: `urn:cap:vector:read:index:name:{n}`,
`urn:cap:vector:write:index:name:{n}` and `urn:cap:vector:read:declaration:index:{n}`. Reading a
passage is never granted here.

## Backends

`flat`, an exact in-process scan with no crate, is the first and default backend. An approximate
backend waits until a measurement says the scan hurts; an external service is mounted by a host
and reached only through the kernel. Every backend meets the same five guarantees (design,
section 2), checked by one conformance test.

## License

MIT or Apache-2.0, at your option.
