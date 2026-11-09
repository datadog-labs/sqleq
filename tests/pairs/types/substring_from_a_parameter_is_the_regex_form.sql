-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: PR #249 review: SUBSTRING read its second argument as a position whatever its type, so
--   substring(s from $1) was one plan with substring(s from $1::integer); Postgres resolves an
--   argument of unknown type, a bare parameter or a string literal, to its regular-expression form
--   substring(text, text), and only an integer selects the position form. An argument that is not an
--   INTEGER is refused (src/lower.rs), and so is a parameter inference typed INTEGER
--   (src/param_types.rs)
-- witness: t = {('abc')}, $1 = '2': A returns NULL, since the pattern 2 matches nothing in abc; B
--   returns 'bc'
create table t (s text);
SELECT substring(s from $1) AS r FROM t;
SELECT substring(s from $1::integer) AS r FROM t;
