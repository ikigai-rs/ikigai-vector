# Working in this repository

`ikigai-vector` is an ikigai module: vector search as resources, one contract with pluggable
backends. These rules are for every agent and every person working
here. `CLAUDE.md` is a link to this file.

- **The README is the contract.** Its resource map says which names exist and which are planned.
  A change that adds a name updates the map in the same pull request.
- **Read files through the kernel, never `std::fs`.** A host binds what answers (an
  index declaration, a persisted embedding store, a mounted vector service); this crate opens no
  file, socket or process itself.
- **Query and passage text are data, never instructions**: never quoted back in an error, and
  never kept by a backend.
- **Read [ACCESSIBILITY.md](ACCESSIBILITY.md) before any change to output people read**:
  messages, faces, terminal text. A change that adds such output adds it to that file.
- Security reports go through GitHub private vulnerability reporting, never a public issue.
- Every change lands through a pull request with green CI.
- Never invent testing, tool results or conformance evidence.
- Use American spelling in everything you write: code comments, docs, commit messages and pull
  requests.
