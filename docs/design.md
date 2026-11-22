# ikigai-vector: one vector contract, pluggable backends

Design note for ledger #3855. **Design only:** nothing here is built, and no code lands with it.
`ikigai-vector` will be its own module and repository. This note lives in `ikigai-knowledge`
until that repository exists, and moves there as the draft of its README.

What ikigai-knowledge needs from it: given a question's text, the names of the passages nearest
to it, so `urn:knowledge:context:question:{h}` can list them beside the linked entities'
neighborhoods. Everything else follows from that need and from the knowledge principles:
singular names, type first, partitions by named keys, freshness from golden threads, and an
answer composed only from what the caller may read.

## 1. The contract

Five names. Each one is singular, has the type first, and is partitioned by named keys.

| Set | Name | Kind | Verb |
|---|---|---|---|
| index declaration | `urn:vector:declaration:index:{n}` | **ATOM** (config) | `source` |
| index | `urn:vector:index:name:{n}` | **ATOM** (a materialized cache) | `source`, `sink` (rebuild) |
| member | `urn:vector:member:index:{n}:passage:{p}` | Sink edge | `sink` (add), `delete` (remove), `exists` |
| embedding | `urn:vector:embedding:model:{m}:hash:{h}` | composition | `source` |
| nearest | `urn:vector:nearest:index:{n}` (`text=`, `k=`) | composition | `source` |

### Index declaration: `urn:vector:declaration:index:{n}`

A Turtle file in the config home, read through the kernel the way ikigai-knowledge reads a source
declaration (`<config home>/vector/index/{n}.ttl`). It says three things:

- **the backend**: `flat` (in-process, exact), and later `hnsw` (in-process, approximate) or
  `service` (an external engine mounted by the host);
- **the embedding model**: a model name as ikigai-llm installs it (`embeddinggemma`), and the
  provider (`ollama`, local, by default);
- **the dimensions** the model is expected to produce, so a model swap that changes them is
  refused at build time rather than at query time.

Which backend answers is configuration, not code: the ikigai-classify pattern. A reader of
`nearest` cannot tell which backend served it, except by the `exact` flag in the answer (below).
The declaration is sealed from models, like a source declaration: a model never picks the
backend or the embedding model.

### Index: `urn:vector:index:name:{n}`

The vectors of a set of passages, held by the declared backend. It is an ATOM only because a
backend is stateful. It is a **cache**, never a source of truth: its inputs are the declaration,
the member list and the embeddings, and `sink` with no body rebuilds it from them.

`source` answers a description, never vectors: the backend, the model and its digest, the
dimensions, the member count, whether it is exact, and the digest of the member list it was built
from. Faces: `text/plain` by default, `text/turtle`. The text face follows ACCESSIBILITY.md: plain
lines of words, one fact per line.

The index records the **model digest** it was built with (ikigai-llm's `urn:llm:embed` answer
names it). If `urn:llm:<provider>:installed` changes the digest, the index's thread is cut and
the index answers as stale until it is rebuilt. Vectors from two digests are never compared.

### Member: `urn:vector:member:index:{n}:passage:{p}`

One edge per passage in the index. Golden threads maintain them; nobody calls them on a schedule:

- **added** (`sink`) when a passage appears in a corpus the index serves. The endpoint reads the
  passage's text through the kernel, embeds it (the embedding name below), and inserts it;
- **removed** (`delete`) when the passage's thread is cut and the passage no longer exists. A
  passage whose text changed has a new range, so it gets a new name: its old member is removed
  and its new one is added;
- `exists` says whether a passage is in the index.

Each add or remove cuts the index's thread, and so the thread of every `nearest` on that index.

### Embedding: `urn:vector:embedding:model:{m}:hash:{h}`

`{h}` is the SHA-256 of the passage text (lowercase hex). `{m}` is the model name. The endpoint
calls `urn:llm:embed model={m}` through the kernel, so it is pure and cacheable on text and model.
It hangs on `urn:llm:config` and `urn:llm:<provider>:installed`, exactly as `urn:llm:embed` does,
and its answer carries the digest. A name with no text behind it cannot be computed from the hash
alone. So the member endpoint, which holds the text, is the caller. A direct `source` of an
embedding name that is not cached is `NotFound`, naming the member endpoint as the way in.

Embeddings are also **persisted** per index (a sink through the kernel to the host's mount), keyed
by `(digest, hash)`. A kernel cache eviction or a restart then costs a rebuild, not a re-embed.
This matters: re-embedding is the expensive step (section 5).

### Nearest: `urn:vector:nearest:index:{n}` (`text=`, `k=`)

The query's text is embedded with the index's model. The backend returns the `k` nearest members,
and the answer is **passage names, never passage text**:

```text
nearest urn:vector:nearest:index:ikigai text="how is a golden thread cut" k=3
3 passages, exact, model embeddinggemma digest 8546…
  1. urn:knowledge:passage:document:…:range:1200-1980  score 0.812345
  2. urn:knowledge:passage:document:…:range:0-640      score 0.799120
  3. urn:knowledge:passage:document:…:range:4410-5122  score 0.799120
```

(The names and scores above illustrate the face's shape. They are not output.)

- `k` is required, from 1 to a bound the declaration sets (default 50). An out-of-range `k` is
  `InvalidArgument`, naming `k` and the bound.
- `text` is data, never instructions, and never quoted back in an error or a trace note. The same
  rule holds in ikigai-llm.
- Faces: `text/plain` (above), `text/turtle` and `application/json`, each listing rank, name and
  score.
- Because nearest answers names, **the caller's capability still decides what is read.** To read
  a passage, the caller sources its name, and the knowledge module checks
  `urn:cap:store:read:graph:<iri>` and its own read scopes there. A name the caller may not read
  is `Denied` on read, never quietly dropped.

## 2. Backends behind one contract

### What every backend must guarantee

These are written as one conformance test that every backend runs (section 6):

1. **Deterministic order, ties included.** Scores are cosine similarities of L2-normalized
   vectors, rounded to 6 decimal places *before* ranking. Order is by rounded score, descending,
   then by passage name, ascending, comparing bytes. Rounding absorbs the last-bit differences
   that a different summation order (SIMD width, thread count) produces. The same index state,
   query and `k` give the same list every time.
2. **Delete is immediate and total.** After `delete` of a member returns, no `nearest` returns
   that name. No tombstone comes back after a compaction, and no stale entry is served from a
   segment.
3. **Rebuild is equivalent.** An index rebuilt from its member list and embeddings answers what
   the incrementally maintained one answers. For an exact backend, the two answers are identical.
   For an approximate one, the answer says `exact=false` and the test checks recall against
   `flat` instead (below).
4. **Exactness is declared.** Every answer says whether it is exact. An approximate backend states
   its recall at the first corpus's size against `flat`, measured by the conformance test,
   and does not assume it.
5. **No text kept.** A backend stores names and vectors only. Passage text never enters a backend,
   so mounting an external service never copies document text out of the kernel.

### In-process first: `flat`, an exact scan with no crate

For the first corpus, the recommendation is **no ANN crate**: an exact flat scan in ikigai-vector
itself. It is a dot product over a contiguous `Vec<f32>` plus a top-k with the tie rule above.
The measurement in section 5 shows it answers in single-digit milliseconds at our size. It is
exact, so guarantee 3 is identity. It is trivially deterministic, delete is a swap-remove, rebuild
is a reload, and it compiles to `wasm32-unknown-unknown` (CI's `wasm-lib` check), which a C++ or
LMDB-backed crate does not.

The first in-process ANN backend (`hnsw`) waits until the scan's cost is measured to hurt. Two
signs of that: an exact query near 100 ms, or the second corpus (the semantic CMS, ledger #3862)
growing the index past a few hundred thousand passages. The candidates, with what each must be
checked for before it is chosen. These are facts from crates.io on 2026-11-20 plus the questions
this note leaves open. None of the four crates has been tried here:

| Crate | Latest | Last release | What to verify before adopting |
|---|---|---|---|
| `hnsw_rs` | 0.3.5 | 2026-10-09 | pure Rust; does it support delete, or only insert (guarantee 2)? wasm build? |
| `instant-distance` | 0.6.1 | 2023-06-26 | pure Rust, but no release since 2023; build-once, so delete means rebuild |
| `usearch` | 2.26.6 | 2026-10-31 | a C++ core behind Rust bindings: not wasm-clean, so a native-only backend |
| `arroy` | 0.9.0 | 2026-10-22 | LMDB-backed (Annoy-like): persistent, but native-only, and it brings its own storage beside the kernel's |

A crate is vetted the way the ecosystem vets any dependency: its license (MIT or Apache-2.0
compatible), its maintenance, whether it builds for the targets the module claims, and whether it
passes the five guarantees in the shared conformance test.

### An external service, mounted later: `service`

A host can bind a vector engine (Qdrant, pgvector, or anything that holds vectors under ids) as a
module answering a small backend contract: upsert, delete, top-k and count. ikigai-vector's
`service` backend reaches it only through the kernel, with `urn:cap:net:<host>` like every
network call. ikigai-vector opens no socket itself. The service must meet the same five
guarantees. In particular, guarantee 1 means ikigai-vector re-ranks the service's answer with the
tie rule. So the service is asked for a few more than `k` results, and a tie that straddles the
boundary is resolved by name, not by the service's internal order.

## 3. Cacheability

`nearest` **hangs on the index's thread**. Every membership change cuts it. That is correct for
nearest itself: it is cheap (section 5), so recomputing it costs little.

It must therefore **never be joined into an expensive cached composite**. If
`urn:knowledge:answer:question:{h}` depended on `nearest` directly, every passage added anywhere in
the corpus would cut every cached answer, and each would cost a model call to recompute. The rule
for ikigai-knowledge (#3859):

- `urn:knowledge:context:question:{h}` calls `nearest` and lists the names it returns. The context
  hangs on the index thread. It is cheap: an embedding (cached on the question text) plus a scan.
- The answer is composed **from the context's value, not its thread**. The answer request carries
  the list of passage names (and neighborhood names) as an argument, so its cache key is the
  question plus that list. The answer hangs on the threads of the *passages it cites*, not on the
  index.
- So adding an unrelated passage recomputes the context (milliseconds). If the nearest list comes
  out the same, the answer is a cache hit. If a cited passage changes, the answer's thread is cut
  through that passage, which is exactly when it should be.

The same rule applies to any other composite that wants nearest: take the names as a value, and
hang on what is read.

## 4. Capability tokens

Per index, singular and keyed. No token spans indexes:

| Token | Grants |
|---|---|
| `urn:cap:vector:read:index:name:{n}` | `nearest` on `{n}`, `source` of the index description, `exists` of a member |
| `urn:cap:vector:write:index:name:{n}` | `sink` and `delete` of members of `{n}`, and rebuild |
| `urn:cap:vector:read:declaration:index:{n}` | `source` of the declaration |

Rules:

- **Reading a passage is never granted here.** `nearest` answers names. Reading what a name holds
  takes the knowledge module's own scopes and ikigai-store's `urn:cap:store:read:graph:<iri>`,
  checked where the passage is read.
- **Names are information, so membership is scoped by the index.** A nearest answer reveals that
  a passage exists, and what it is about from its score. So grant `read:index:{n}` only to callers
  who may know every member's name. When callers differ in what they may read, give them
  different indexes: one index per corridor (a corpus × a readable set of graphs). One shared
  index whose answers are filtered afterward is not allowed: the knowledge principles say there is
  nothing to filter afterward, and that holds here too.
- **Members are written by the golden-thread maintainer**, a principal holding `write:index:{n}`,
  never by a model. Embedding also needs the embedding provider's grants (`urn:cap:net:<host>` and
  `urn:cap:llm:use:<provider>` when it applies). These belong to the maintainer, not to the
  reader. A reader's `nearest` embeds the query text, so the reader's call needs the same
  provider grants. Otherwise the host delegates the query embedding to the maintainer. That
  choice is left to #3859.

## 5. Measurements

Measured on 2026-11-20 by the spike, on an Apple M5 Pro (18 cores, 64 GB), with `rustc 1.99.1`.
The methods are written out below so anyone can re-run them.

### The first corpus's size

The first corpus is our repositories, docs and work ledger. Sizes come from the GitHub trees API
(`gh api repos/ikigai-rs/{repo}/git/trees/HEAD?recursive=1`, blob sizes) over the 61
repositories of `ikigai-rs` that are not archived. No tree was truncated. Test fixture corpora
(`tests/corpus/`), the vendored `crates/xrust/` and `vendor/` were excluded, because they repeat
other files or are not ours:

| Kind | Files | Bytes |
|---|---|---|
| Markdown (`.md`) | 701 | 8,215,269 |
| Rust (`.rs`) | 1,827 | 41,809,372 |
| Turtle and TOML (`.ttl`, `.toml`) | 411 | 2,070,080 |
| **Total** | **2,939** | **52,094,721** |

**The work ledger was not measured.** The spike had no read access to the ledger's text. Its ids
reach at least 3855, so it holds up to a few thousand items plus their comments. Its size is the
first open measurement below.

The chunking rule is not settled (`urn:knowledge:chunking:name:{n}`, #3857). As a planning figure,
passages of about 2,000 bytes with no overlap give **about 26,000 passages** for the repositories
and docs. That is 52.1 MB / 2,000 bytes, an assumption about chunk size, not a measurement.

### Query cost: an exact in-process scan

A benchmark (single-threaded, no SIMD intrinsics, no crate) built the flat backend's inner loop:
dot products over L2-normalized random `f32` vectors of 768 dimensions (embeddinggemma's
dimensions as ikigai-llm reports them), then a full sort with the tie rule, then the top 10. It was
averaged over 50 queries, with `opt-level = 3`:

| Passages | Vector bytes (`f32`) | Per query |
|---|---|---|
| 26,000 | 79.9 MB | 7.63 ms |
| 50,000 | 153.6 MB | 14.60 ms |
| 100,000 | 307.2 MB | 29.26 ms |

The cost is linear, about 0.29 µs per passage. The full sort is the naive choice: a partial
selection and several threads would lower these numbers, but they are already small. At the first
corpus's size an exact scan costs less than the query's own embedding will (below), so the `flat`
recommendation stands. The figures are for the scan alone: they exclude embedding the query text
and the kernel's dispatch.

### Embedding cost: not measured

**Ollama is not installed on the machine this spike ran on, so no embedding was timed.** No number
is given here for it, and none should be assumed. The measurement to run on a host with Ollama and
`embeddinggemma` pulled:

1. Embed 1,000 passages of the planning size (about 2,000 bytes, drawn from the corpus above)
   through `urn:llm:embed model=embeddinggemma`, one at a time and then in batches if ikigai-llm
   gains batching. Record wall-clock per passage, the model digest and the machine.
2. Full build cost ≈ passages × per-passage time. Incremental cost per golden-thread change ≈
   (passages added) × per-passage time. Query embedding cost = one short text.
3. Disk for persisted embeddings: passages × 768 × 4 bytes (about 80 MB at 26,000 passages),
   plus names.

The knowns: the build is dominated by embedding, not by the index. A model change (a new digest)
means a full re-embed, so embeddings are persisted (section 1). And day-to-day freshness costs only
the changed passages.

## 6. How it is tested, once built

- **One backend conformance test**, run by every backend against the same fixtures:
  - ties: many members with identical vectors, so the name order decides;
  - delete then query;
  - rebuild equals incremental (exact backends) or meets the declared recall against `flat`
    (approximate ones);
  - no text stored;
  - a `k` out of range;
  - a digest mismatch refused.
- `ikigai-conformance` for the module recipe, as every module runs.
- The text faces pinned exactly, and added to ikigai-vector's ACCESSIBILITY.md.

## 7. Decisions this note asks for

1. `flat` (an exact scan, no crate) as the first and default backend. ANN only when a measurement
   says so.
2. The index declaration name, `urn:vector:declaration:index:{n}`, a config ATOM, added to the
   resource map.
3. Score rounding to 6 decimal places before ranking, ties by passage name: the determinism rule
   every backend meets.
4. One index per corridor (a corpus × a readable set of graphs), never one filtered index.
5. Answers composed from nearest's names as a value, so the answer never hangs on the index's
   thread.
6. The open measurement: embedding cost with Ollama (`embeddinggemma`) on a host that has it, and
   the ledger's size.
