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
-- origin: the control for comparing tails as Postgres resolves names: a quoted name keeps its case,
--   so `"Id"` is not `id`
-- witness: $1 = 1, $2 = 'x': A returns (NULL), the column "Id" of the new row; B returns (1)

-- Two columns whose names differ only in case. Only the unquoted spelling folds, so the two
-- RETURNING clauses read different columns.
CREATE TABLE users (id int PRIMARY KEY, "Id" int, name text);
INSERT INTO users (id, name) VALUES ($1, $2) RETURNING "Id";
INSERT INTO users (id, name) SELECT * FROM unnest($1::int[], $2::text[]) RETURNING id;
