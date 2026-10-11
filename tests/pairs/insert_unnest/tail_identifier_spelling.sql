-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: no-counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: proved-gather
-- binding: gather
-- origin: the Lean axis compared the two sides' tails as written, so the same conflict clause and
--   RETURNING spelled with other quoting or case was refused as a different tail

-- One tail, spelled two ways: quoted and unquoted names, `EXCLUDED` in upper case, an alias in
-- another case. Postgres resolves both spellings to the same names.
CREATE TABLE accounts (id int PRIMARY KEY, name text);
INSERT INTO accounts AS A (id, name) VALUES ($1, $2), ($3, $4)
  ON CONFLICT ("id") DO UPDATE SET "name" = excluded."name" RETURNING A.ID, "name" AS Label;
INSERT INTO accounts AS a (id, name) SELECT * FROM unnest($1::int[], $2::text[])
  ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name RETURNING "a"."id", name AS "label";
