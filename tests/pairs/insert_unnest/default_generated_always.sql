-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- binding: gather-generated
-- origin: Postgres accepts DEFAULT in a GENERATED ALWAYS identity, but not an explicit value
-- witness: $1 = 'x': A inserts (1, x); B is given A's generated id, [1], and is rejected (428C9:
--   cannot insert a non-DEFAULT value into column id)
CREATE TABLE accounts (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, name text);
INSERT INTO accounts (id, name) VALUES (DEFAULT, $1);
INSERT INTO accounts (id, name) SELECT * FROM unnest($1::int[], $2::text[]);
