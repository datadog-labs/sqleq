-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #250, the negative side: a parameter cast to integer selects Postgres's position form
--   substring(text, integer) in every spelling, so the quoted call and the SUBSTRING syntax both still
--   lower rather than being refused. They do not meet yet: the ordinary call's result is typed opaque,
--   as every built-in call's is (#239), and the syntax's is a text
-- argument: both sides are substring(s, $1::integer), the characters of s from position $1 on
create table t (s text);
SELECT "substring"(s, $1::integer) AS r FROM t;
SELECT substring(s from $1::integer) AS r FROM t;
