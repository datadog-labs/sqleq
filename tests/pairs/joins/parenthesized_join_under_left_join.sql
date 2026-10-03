-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect lean: unsupported
-- origin: a parenthesized join is lowered on its own, and under an outer join its parentheses must
--   survive, since the grouping changes the result (#31)
-- witness: u = {(1)}, v = w = {}: A keeps u's row, null-extended; in B the inner join with w drops
--   it
CREATE TABLE u (a INTEGER);
CREATE TABLE v (a INTEGER);
CREATE TABLE w (a INTEGER);
SELECT u.a FROM u LEFT JOIN (v JOIN w ON v.a = w.a) ON u.a = v.a;
SELECT u.a FROM u LEFT JOIN v ON u.a = v.a JOIN w ON v.a = w.a;
