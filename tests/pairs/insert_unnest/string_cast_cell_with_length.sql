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
-- expect cvc5: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: a cast through a string type with a length truncates, so the cell is not its parameter
-- witness: $1 = 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', $2 = 1: A truncates to 'a0eebc99', which is
--   not a uuid, and raises 22P02; B inserts the row
CREATE TABLE tk (id uuid PRIMARY KEY, n int);
INSERT INTO tk (id, n) VALUES (CAST($1::varchar(8) AS uuid), $2);
INSERT INTO tk (id, n) SELECT * FROM unnest($1::uuid[], $2::int4[]);
