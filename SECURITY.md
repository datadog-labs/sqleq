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

* Memory-safety or crash bugs in the frontend reachable from ordinary SQL input — it parses
  attacker-influenced text in some pipelines.
* Command injection or path traversal through a corpus file, a DDL string, or an output path.
* A way to make the tool write outside the output directory it was given.

Things that are working as intended, and not vulnerabilities:

* **`sqleq-fuzz` executes the SQL you give it.** That is the whole method: it builds real tables in
  an in-process DuckDB and runs both queries against them. Pointing it at untrusted SQL is
  equivalent to running that SQL.
* The frontend invokes the external solver binary named by `$QED_PROVER`, and the SQLSolver axis
  runs `sqleq-solver` (or, with `--sqlsolver-jvm`, a JVM from `$SQLEQ_SQLSOLVER`). All are
  paths you supply or binaries you build.

## Soundness bugs

A pair where a prover reports *equivalent* and the two queries are not equivalent is the most
serious defect this project can have, but it is a correctness bug, not a security one — it is not
a path to code execution or data access. Report it as a normal GitHub issue, with the two queries
and the DDL. See [CONTRIBUTING.md](CONTRIBUTING.md).
