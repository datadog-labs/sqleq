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
sqleq-check names the axes its verdict rests on.
-->

## Why they are not equivalent

<!--
A counterexample -- a database on which the two queries return different results -- if you have
one. An argument is welcome too.
-->

## Environment

<!-- The sqleq commit, and the prover or SQLSolver version. -->
