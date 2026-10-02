# Contributing

Thanks for looking. Bug reports, fixes, docs and features are all welcome.

## Before you start

- **Security problems:** not here — see [SECURITY.md](SECURITY.md).
- **Bigger changes** (new storage backend, new secret engine, API changes):
  open an issue first so we can agree on the shape before you write it.
- Read [CLAUDE.md](CLAUDE.md). Despite the name it is the contributor guide:
  the layout, the conventions and the **security invariants** that must not break.

## Setup

You need Rust (stable), [Bun](https://bun.sh) and `make`. `protoc` is vendored.

```bash
cp backend/.env.example backend/.env    # STORAGE=raft needs no database
make dev                                 # backend :8200 + frontend :5174
```

`make redis` starts a local Redis if you want `STORAGE=redis`; `make raft-dev`
starts a 3-node cluster.

## Making a change

1. Branch from `main`.
2. Match the surrounding code. Handlers stay thin; logic lives in the modules.
3. API changes start in `proto/timika/v1/*.proto`, then `make gen`.
4. New behaviour gets an end-to-end scenario in `frontend/e2e/scenarios/`
   (see [frontend/e2e/README.md](frontend/e2e/README.md)).
5. Update the docs in `docs/` when behaviour changes.
6. Run both before opening a pull request:

   ```bash
   make test    # cargo test + type checks
   make e2e     # every scenario on real processes, about a minute
   ```

## Pull requests

- One topic per pull request, with a description of what changed and why.
- Features must work on **both** storage backends (Raft and Redis).
- Anything that sends a command to a server must validate every value and pass
  it as one quoted word — never a user-supplied command line.
- By contributing you agree that your work is licensed under the
  [MIT License](LICENSE).

## Conduct

Be kind and assume good intent. See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
