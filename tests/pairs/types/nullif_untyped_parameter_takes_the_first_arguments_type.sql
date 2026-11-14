-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #251: #249 refused NULLIF's second argument unless it was a string literal, a bare NULL or a
--   value of the first's type, and a parameter nothing typed is none of those to the frontend. Postgres types it
--   as the first argument (PREPARE reports parameter_types = {bigint} here), so it takes that type
-- argument: a > 0 implies a > -1, so the two filters keep the same rows, and the select lists are one text
create table t (a integer, b integer);
SELECT a, NULLIF(SUM(b), $1) AS r FROM t WHERE a > 0 AND a > -1 GROUP BY a;
SELECT a, NULLIF(SUM(b), $1) AS r FROM t WHERE a > 0 GROUP BY a;
