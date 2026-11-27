-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #311: a quoted name in lower case is the type the unquoted name is, so the two casts still share a symbol
-- argument: Postgres reads the two targets as one type, so the two statements are the same query
CREATE TYPE mood AS ENUM ('a', 'b');
CREATE TYPE "Mood" AS ENUM ('b', 'a');
CREATE TABLE t (id int PRIMARY KEY, m text, n text);
SELECT id FROM t WHERE m::"mood" < n::"mood";
SELECT id FROM t WHERE m::mood < n::mood;
