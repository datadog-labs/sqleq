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
-- catalog: inferred-seeded
-- origin: issue #250: the quoted, ordinary-call spelling "substring"(s, $1) lowered to SUBSTRING with
--   its parameter read as a position, typed INTEGER for the whole pair by B's cast, so the two sides
--   were one plan; Postgres resolves an argument of unknown type to the regular-expression form
--   substring(text, text). Refused since #249 (src/param_types.rs), which this pin holds
-- witness: t = {('abc')}, $1 = '2': A returns NULL, since the pattern 2 matches nothing in abc; B
--   returns 'bc'
create table t (s text);
SELECT "substring"(s, $1) AS r FROM t;
SELECT "substring"(s, $1::integer) AS r FROM t;
