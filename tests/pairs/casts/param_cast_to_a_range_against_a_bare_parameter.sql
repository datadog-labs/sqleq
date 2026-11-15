-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #285: with the cast dropped, ($1)::int4range was the bare $1 of the other side, which Postgres
--   types text in a select list
-- witness: t = {(1)}, $1 = '[1,4]': A returns the range [1,5) and B the text [1,4]
CREATE TABLE t (id int PRIMARY KEY);
SELECT id, ($1)::int4range AS x FROM t;
SELECT id, $1 AS x FROM t;
