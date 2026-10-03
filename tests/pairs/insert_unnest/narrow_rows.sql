-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: invalid-sql
-- binding: gather
-- origin: the Lean axis's fragment boundary: invalid SQL is reported, not proved
-- witness: Postgres rejects A whatever its parameters (more target columns than expressions); B
--   with $1 = '{1}' inserts a row

-- One parameter per row where the syntax needs one per column: Postgres rejects the VALUES side.
CREATE TABLE t (a int, b text, c int);
INSERT INTO t (a, b, c) VALUES ($1), ($2), ($3);
INSERT INTO t (a, b, c) SELECT * FROM unnest($1::int[], $2::text[], $3::int[]);
