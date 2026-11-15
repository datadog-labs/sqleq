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
-- origin: issue #299: a bare parameter among a statement's select items states no type to inference, so
--   ($1)::bigint was dropped and was the bare $1 of the other side, which Postgres makes a text
-- witness: t = {(1)}, $1 = '07': A returns the bigint 7 and B the text 07
CREATE TABLE t (id int PRIMARY KEY);
SELECT id, ($1)::bigint AS x FROM t;
SELECT id, $1 AS x FROM t;
