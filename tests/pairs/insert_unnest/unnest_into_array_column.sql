-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: a gather pair the rule cannot bind (generated cells) whose unnest side Postgres will not
--   prepare as written; sqleq-fuzz's rejection probe
-- witness: Postgres refuses to prepare the unnest side, whatever its arrays: unnest yields text
--   scalars, and column tags is text[] (42804). The VALUES side inserts its rows.

CREATE TABLE s (id serial PRIMARY KEY, tags text[]);
INSERT INTO s (id, tags) VALUES (DEFAULT, $1), (DEFAULT, $2);
INSERT INTO s (id, tags) SELECT * FROM unnest($1::int[], $2::text[]);
