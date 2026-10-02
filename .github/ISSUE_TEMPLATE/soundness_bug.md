---
name: Soundness bug
about: A prover reported two queries equivalent, and they are not
title: '[soundness] '
labels: ''
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

<!-- Which axis claimed equivalence (qed or sqlsolver), and the command you ran. -->

## Why they are not equivalent

<!--
A counterexample -- a database on which the two queries return different results -- if you have
one. An argument is welcome too.
-->

## Environment

<!-- The sqleq commit, and the prover or SQLSolver version. -->
