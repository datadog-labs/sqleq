# Changelog

Notable changes to sqleq, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): before 1.0, a minor version may change
the command line, the IR or the library's API.

## [Unreleased]

### Changed

- **The `duckdb` feature links DuckDB 2.0.1,** where it linked 1.5.6 (the `duckdb` crate, now
  1.20001). A verdict from `--engine duckdb` is a claim about the engine's own semantics, and
  names the version. The build puts the library in `target/<profile>/`, beside the binaries, where
  it was under `deps/`.
- **The cvc5 axis needs a cvc5 where an empty table has no group** (cvc5/cvc5#13151), and
  `tools/build_cvc5.sh` builds one, `28dd0643` on `bags-algebra-partition`. `GROUP BY` is now a
  bare `table.aggr`, and an aggregate without it a fold, one row also over an empty table. On the
  previous cvc5 this encoding gives false proofs, so `cargo build --features cvc5` refuses a cvc5
  the script installed at another commit: run the script again after a pull that moves the pin.
  The axis's answers are not comparable with the previous pin's: the pinned pairs
  `joins/self_join_on_a_key_is_not_distinct_from`, `windows/rank_partitioned` and
  `windows/rank_two_keys` are no longer proved.

### Fixed

- **A build from a checkout with cargo 1.100 or later finds the PostgreSQL it fetched.** Cargo
  1.100 changed where a build script's output goes, and the build script counted directories up
  from it: the PostgreSQL landed under `target/<profile>/build/`, where `sqleq-fuzz` and its tests
  did not look, so they fell back to the one on `PATH` or reported none. The `duckdb` feature did
  not build on that cargo at all before the move to DuckDB 2.0 above.

## [0.1.0] - 2026-11-13

The first release, published on crates.io as `sqleq`: `cargo install --locked sqleq`.

sqleq decides whether two SQL queries are equivalent. It aims to return a proof when they are and a
concrete counterexample when they are not, by independent machinery, so each checks the other.
Query equivalence is undecidable in general, and many pairs come back undecided.

### Added

- **`sqleq-check`**, the command you run. It asks the backends you choose about each pair (files, a
  directory of them, or a corpus CSV) and reports what each answered. With `--portfolio` it runs them
  on each pair at once, under one deadline, for one combined verdict. A pair one backend proves and
  another refutes is an `alarm`, and fails the run. `--expect` policies make a directory of rewrites
  you expect to be safe into a CI test.
- **`sqleq-frontend`** reads PostgreSQL queries and DDL. It resolves names against the schema, types
  the expressions, and lowers each pair to an IR that provers read. The IR carries the schema's keys
  and nullability. It covers `SELECT` pairs (joins, grouping, subqueries, set operations, ordering
  with limits) and pairs of `INSERT`, `UPDATE` or `DELETE` statements. What it cannot lower
  faithfully it refuses, with the reason, rather than approximate.
- **`sqleq-solver`**, the default prover. It is a Rust rewrite of
  [SQLSolver](https://github.com/SJTU-IPADS/SQLSolver)'s proof engine over the frontend's IR, with
  Z3 compiled from source and linked in.
- **`sqleq-fuzz`**, the refuter. It runs both statements on PostgreSQL 17 over generated tables that
  honour the schema's constraints, and reports a difference as the rows that show it. DuckDB is a
  second engine, behind the `duckdb` feature.
- **Optional axes:**
  - the [QED](https://github.com/qed-solver/prover) prover, an external binary, as a second opinion
    (`--qed`);
  - the Lean axis (the `lean` feature, from a checkout). It proves pairs of `INSERT … VALUES` against
    `INSERT … SELECT * FROM unnest(…)`, and the Lean kernel checks each proof;
  - an experimental cvc5 axis (the `cvc5` feature, from a checkout).

[Unreleased]: https://github.com/datadog-labs/sqleq/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/datadog-labs/sqleq/releases/tag/v0.1.0
