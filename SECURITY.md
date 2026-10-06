# Security Policy

## Reporting a vulnerability

Please do not open a public GitHub issue for a security vulnerability.

Report it to **security@datadoghq.com**. Datadog's disclosure process and PGP key are at
<https://www.datadoghq.com/security/>. Include enough to reproduce: the version or commit, the
input, and what you observed.

## What counts

`sqleq` is a developer tool that reads SQL and DDL and runs external solvers. The inputs it is
designed to handle are queries you already have; it is not a sandbox and it is not a boundary.
Things we would want to know about:

* Memory-safety or crash bugs in any of the binaries reachable from ordinary SQL input — the
  frontend parses attacker-influenced text in some pipelines.
* Command injection or path traversal through a corpus file, a DDL string, or an output path.
* A way to make a binary write somewhere other than the places listed below.

Things that are working as intended, and not vulnerabilities:

* **`sqleq-fuzz` executes the SQL you give it.** That is the whole method: it builds real tables in
  an in-process DuckDB and runs both statements against them. Pointing it at untrusted SQL is
  equivalent to running that SQL — and `sqleq-check --portfolio` runs `sqleq-fuzz` by default, as
  does any run whose `--axes` names `fuzz`.
* **`sqleq-check` starts other programs.** It runs every backend as a subprocess: `sqleq-frontend`,
  the QED prover (`--prover`, `$QED_PROVER`, `PATH`, or the newest Nix-built prover in
  `/nix/store`), `sqleq-solver`, `sqleq-fuzz` and `sqleq-lean`, each found as
  [CONTRIBUTING.md](CONTRIBUTING.md) describes. With `--sqlsolver-jvm` it also compiles the bridge
  in `tools/sqlsolver/` with `javac` and runs `java`, both from `PATH`, against the fork tree you
  name. `sqleq-lean` runs `lake` (`$LAKE`, or the one on `PATH`), which builds the `lean/` package
  and runs `lean` on the files it generates. The frontend itself runs nothing.
* **Writes outside an output directory you named.** Temporary directories under `$TMPDIR`, removed
  afterwards unless you pass `--keep`. `sqleq-check --bless` rewrites, by design, the `expect` lines
  of each pair file whose pins moved; a file with a header error is left alone. `--sqlsolver-jvm` writes the compiled bridge to `tools/sqlsolver/out-fork/`,
  and `sqleq-lean` writes the Lean build to `lean/.lake/`. `sqleq-frontend pair.sql` without an
  output path writes `pair.fe.json` beside its input, and `sqleq-fuzz csv` without one writes
  `<corpus>.fuzz.json` beside its corpus.

## Soundness bugs

A pair where a prover reports *equivalent* and the two queries are not equivalent is the most
serious defect this project can have, but it is a correctness bug, not a security one — it is not
a path to code execution or data access. Report it as a normal GitHub issue, with the two queries
and the DDL. See [CONTRIBUTING.md](CONTRIBUTING.md).
