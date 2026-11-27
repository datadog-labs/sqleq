-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #311, for a name: the symbol's key was in lower case, so a cast to "Mood" was the cast to mood, and
--   the two are different types
-- witness: t = {(1, 'a', 'b')}: mood orders a before b, so A returns 1. "Mood" orders b before a, so B returns no row
CREATE TYPE mood AS ENUM ('a', 'b');
CREATE TYPE "Mood" AS ENUM ('b', 'a');
CREATE TABLE t (id int PRIMARY KEY, m text, n text);
SELECT id FROM t WHERE m::mood < n::mood;
SELECT id FROM t WHERE m::"Mood" < n::"Mood";
