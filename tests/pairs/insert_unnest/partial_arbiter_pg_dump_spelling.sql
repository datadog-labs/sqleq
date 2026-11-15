-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: no-counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: proved-gather
-- binding: gather
-- origin: issue #212: the index as pg_dump writes it, each test and the whole predicate in parentheses, and the
--   target as a client writes it, the tests the other way round and in another case on one side
-- argument: Postgres infers the index when the target's predicate implies the index's, and each of the
--   index's two tests is one of the target's, so both sides name it; under the gather rule they propose the
--   same rows in the same order
CREATE TABLE s (k int, v int, x int, y timestamp, n text);
CREATE UNIQUE INDEX s_live ON s USING btree (k, v) WHERE ((x IS NOT NULL) AND (y IS NULL));
INSERT INTO s (k, v, x, n) VALUES ($1, $2, $3, $4), ($5, $6, $7, $8)
  ON CONFLICT (k, v) WHERE y IS NULL AND x IS NOT NULL DO UPDATE SET n = excluded.n;
INSERT INTO s (k, v, x, n) SELECT * FROM unnest($1::int[], $2::int[], $3::int[], $4::text[])
  ON CONFLICT (K, V) WHERE Y is null and X is not null DO UPDATE SET n = EXCLUDED.n;
