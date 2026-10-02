-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqlsolver-rust: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: proved-gather
-- binding: gather
-- origin: the gather rule with the unnest side first, the CAST spelling, and VALUES cells cast to
--   their own column's type

-- The unnest side first, CAST spelling, and VALUES cells cast to their own column's type.
CREATE TABLE t (a integer, b text);
INSERT INTO t (a, b) SELECT * FROM UNNEST(CAST($1 AS INT[]), CAST($2 AS TEXT[]));
INSERT INTO t (a, b) VALUES ($1::int4, $2), (NULL, $4::text), ($3, NULL);
