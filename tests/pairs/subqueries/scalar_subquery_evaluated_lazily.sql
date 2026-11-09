-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: unsupported
-- expect lean: unsupported
-- origin: PR #209 review: the cvc5 axis asserted that every scalar subquery has at most one row over the
--   whole database, but Postgres evaluates an uncorrelated one only where it is needed, so a database
--   on which it has two rows still runs A; a scalar subquery that may have more than one row is
--   refused (sqleq-cvc5/src/translate.rs)
-- witness: u = {(5)}, t = {(1), (2)}: A returns 5, B returns no rows
create table u (x integer);
create table t (a integer);
SELECT COALESCE(u.x, (SELECT t.a FROM t)) AS v FROM u;
SELECT COALESCE(u.x, (SELECT t.a FROM t)) AS v FROM u WHERE (SELECT COUNT(*) FROM t) < 2;
