-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #123: range_agg keeps one member of a class of equal ranges, the order a sorted subquery hands
--   on decides which, and the two queries lowered to one plan, which lifted the refusal of the cast to text
-- witness: u = {(1, '[1.0,2.0)'), (2, '[1.00,2.00)')}: on Postgres 17, A returns '{[1.00,2.00)}' and B '{[1.0,2.0)}'
create table "u" ("k" INTEGER, "n" NUMRANGE);
SELECT CAST(range_agg(n) AS TEXT) FROM (SELECT n FROM u ORDER BY k) s;
SELECT CAST(range_agg(n) AS TEXT) FROM (SELECT n FROM u ORDER BY k DESC) s;
