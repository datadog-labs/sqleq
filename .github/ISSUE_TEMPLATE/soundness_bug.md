---
name: Soundness bug
about: sqleq reported two queries equivalent, and they are not
title: '[soundness] '
labels: soundness
assignees: ''

---

<!--
This is the most serious kind of bug this project can have, so thank you for reporting it. The
pair does not need to be minimized: the fix minimizes it and pins it under tests/pairs/.
-->

## The two queries

```sql
-- query A

-- query B
```

## Schema (DDL)

```sql
```

## What was reported

<!--
Which axis claimed equivalence, and the command you ran. The claim can come from a prover (`qed`,
`sqleq-solver` or `sqlsolver-jvm`) or from the frontend alone: `emit-reflexive` (both queries lowered
to the same plan) or `reflexive` (refused, but both normalize to the same query). Under `--portfolio`,
sqleq-check names the axes its verdict rests on. A Lean proof (`proved-gather`) is a claim too, under
its own rule for parameters. The `--json` output for the case, if you have it, carries every axis's
raw answer.
-->

## Why they are not equivalent

<!--
A counterexample -- a database on which the two queries return different results -- if you have
one. An argument is welcome too.
-->

## Environment

<!--
The sqleq commit (sqleq-solver and sqleq-lean are built from it), and the version of any backend
built elsewhere: the QED prover's revision, the JVM SQLSolver fork's revision, or the Lean
toolchain.
-->
