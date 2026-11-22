# ikigai-vector

Vector search as [ikigai](https://github.com/ikigai-rs) resources: one contract, pluggable
backends. Given a question's text, it answers the **names** of the nearest passages, never their
text, so reading what a name holds stays under the caller's own capability.

The design is [docs/design.md](docs/design.md); this README is the contract that follows it.

## Resource map

| Set | Name | Kind | Verb | Status |
|---|---|---|---|---|
| index declaration | `urn:vector:declaration:index:{n}` | ATOM (config) | `source` | built |
| index | `urn:vector:index:name:{n}` | ATOM (a materialized cache) | `source`, `sink` (rebuild) | built |
| member | `urn:vector:member:index:{n}:passage:{p}` | Sink edge | `sink`, `delete`, `exists` | built |
| embedding | `urn:vector:embedding:model:{m}:hash:{h}` | composition | `source` | built |
| nearest | `urn:vector:nearest:index:{n}` (`text=`, `k=`) | composition | `source` | built |
| change thread | `urn:vector:change:index:{n}` | Sink edge (no data) | `sink` | built |
| persisted embeddings | per index, a sink through the kernel to the host's mount, keyed by `(digest, hash)` | ATOM | | planned |

- **The index declaration** is the Turtle file `<config home>/vector/index/{n}.ttl`, read through
  the kernel as `urn:vector:file:path:index/{n}.ttl`, which the host binds over the config home's
  `vector` directory (with `ikigai-fs`, say). `<>` names the declaration itself:

  ```turtle
  @prefix vector: <https://ikigai-rs.dev/ns/vector#> .
  <> a vector:IndexDeclaration ;
     vector:backend "flat" ;          # optional: flat is the default
     vector:model "embeddinggemma" ;  # required: the model as ikigai-llm installs it
     vector:provider "ollama" ;       # optional: without it, urn:llm:embed's default
     vector:dimensions 768 ;          # required: what the model is expected to produce
     vector:maxK 50 .                 # optional: the bound on k, 50 by default
  ```

  A blank node is refused, and so is a malformed value. Faces: `text/plain` (default) and
  `text/turtle`, which states the defaults too.
- **The index** answers a description, never vectors: the backend, exact or approximate, the
  model and the digest its vectors were made with, the dimensions, the member count, the digest of
  the member list (`sha256:` over each name and a newline, in byte order) and the bound on `k`.
  Faces: `text/plain` (default) and `text/turtle`. `sink` with no body rebuilds it from its
  declaration, its member list and their embeddings; with no members yet, it builds an empty one.
- **A member** `sink` reads the passage's text through the kernel under the caller's capability,
  embeds it with the index's model and inserts it. `delete` removes it at once (idempotent: a
  passage that is not a member says so and changes nothing). `exists` answers `true` or `false`.
- **The embedding** is `urn:llm:embed model={m}` over a text whose SHA-256 is `{h}`. The text is
  passed as `text=` and checked against the hash; without it the name is `NotFound`, naming the
  member endpoint as the way in, since a hash alone cannot be embedded. `application/json`.
  Cacheable on the text and the model, and it carries `urn:llm:embed`'s threads.
- **Nearest** embeds the query with the index's model and answers passage names with rank and
  score. Faces: `text/plain` (default), `text/turtle`, `application/json`:

  ```text
  2 passages, exact, model embeddinggemma digest sha256:…
    1. urn:knowledge:passage:document:…:range:1200-1980  score 0.812345
    2. urn:knowledge:passage:document:…:range:0-640  score 0.799120
  ```

  `k` runs from 1 to the declaration's bound; out of range is `InvalidArgument` naming `k` and the
  bound. The query text is never quoted back in an error or a trace note.
- **The change thread** exists because the kernel cuts only the thread named after a write's
  target, and an endpoint cannot cut another one. Every member change and rebuild also writes
  `urn:vector:change:index:{n}` (as ikigai-store writes `urn:iki:store:graph-written:{graph}`),
  and nearest, the description and `exists` hang from it. It changes no data.
- **Persisted embeddings** are planned, not built (design, section 1): today an index lives in the
  process, and a restart costs re-adding its members, whose embeddings come back from the kernel's
  cache while it holds them, or from the model.

## Two digests are never compared

The index records the model digest its vectors were made with. A member whose embedding carries
another digest is refused (`Conflict`), and so is a query, each naming both digests; a rebuild
re-embeds every member at the current digest. A declaration that changes the model, the provider,
the backend or the dimensions is refused the same way until the index is rebuilt; a changed bound
on `k` takes effect at the next member change or rebuild.

## Capabilities

Per index, never spanning indexes; each endpoint declares the family and refuses another index by
its own rule with a typed `Denied`:

| Token | Grants |
|---|---|
| `urn:cap:vector:read:index:name:{n}` | `nearest` on `{n}`, the index description, a member's `exists` |
| `urn:cap:vector:write:index:name:{n}` | a member's `sink` and `delete`, the rebuild, the change thread |
| `urn:cap:vector:read:declaration:index:{n}` | the declaration |

The endpoints that read the declaration file (the declaration, a member `sink`, the rebuild) also
declare `urn:cap:fs:read:*`, since that read is the caller's and the conventional mount judges it
by path. **Reading a passage is never granted here.** One index per corridor (a corpus and a
readable set of graphs), never one index filtered afterward (design, section 4).

## Caching: take nearest's names as a value

Nearest hangs on its index's thread, so every member change recomputes it; it is cheap (an
embedding cached on the query text, then a scan). **Never join it into an expensive cached
composite**: a composite takes nearest's names as a VALUE (an argument), so its cache key is the
list of names and it hangs from the passages it reads, not from the index (design, section 3).

## Backends

`flat`, an exact in-process scan with no crate, is the first and default backend: a dot product
over one contiguous `Vec<f32>`, scores rounded to 6 decimal places before ranking, ties by passage
name. It compiles for `wasm32-unknown-unknown`. A host offers another kind with
`Vectors::with_backend`, and every backend meets the same five guarantees, checked through the
kernel by `tests/backend.rs`. An approximate backend waits until a measurement says the scan hurts;
the recall check against `flat` that it would need is not built yet. An external service is mounted
by a host and reached only through the kernel.

## Using it

```rust
use std::sync::Arc;
use ikigai_core::{Fallback, Kernel, Space};

let vectors = ikigai_vector::Vectors::new();
let spaces: Vec<Arc<dyn Space>> = vec![
    Arc::new(vectors.space()),
    // beside it: a file module at urn:vector:file:path:{path}, ikigai-llm's urn:llm:embed,
    // and whatever serves the passages
];
let kernel = Kernel::new(Arc::new(Fallback::new(spaces)));
```

## Vocabulary

`src/vocabulary.ttl` defines every term a declaration or an answer uses, under
`https://ikigai-rs.dev/ns/vector#`. It is not deployed at that namespace yet.

## License

MIT or Apache-2.0, at your option.
