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
-- origin: a stored generated column takes DEFAULT, and rejects any other value
-- witness: $1 = 2: A inserts (2, 4); B is given A's generated value, [4], and is rejected (428C9:
--   cannot insert a non-DEFAULT value into column twice)
CREATE TABLE sizes (n int, twice int GENERATED ALWAYS AS (n * 2) STORED);
INSERT INTO sizes (n, twice) VALUES ($1, DEFAULT);
INSERT INTO sizes (n, twice) SELECT * FROM unnest($1::int[], $2::int[]);
