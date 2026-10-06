# `tools/`

| file | what it does |
|---|---|
| `update_license_3rdparty.sh` | regenerates `LICENSE-3rdparty.csv`; `--check` is the CI gate — see [`../CONTRIBUTING.md`](../CONTRIBUTING.md) |
| `license-3rdparty-extra.csv` | the attribution rows for components that are not crates, appended by the script above |
| `sqlsolver/` | our side of the IR bridge to the JVM SQLSolver, the deprecated backup cross-check of `sqleq-solver`; `sqleq-check --sqlsolver-jvm` compiles it — see [`../docs/SQLSOLVER.md`](../docs/SQLSOLVER.md) |
| `lean_replay.py` | re-runs the Lean axis's proved `INSERT` pairs on a real Postgres, as an independent check — see [`../docs/LEAN.md`](../docs/LEAN.md). Python 3 (standard library only) and `psql`; never run by CI |

There is no harness script here. Pairs are run by [`sqleq-check`](../sqleq-check/README.md): a
directory of `.sql` pairs or a corpus CSV (`--corpus`) in, verdicts and a CI exit code out, and the
pinned-pair suite behind `--expect pinned`. The link check is `tests/doc_links.rs`, which
`cargo test` runs.
