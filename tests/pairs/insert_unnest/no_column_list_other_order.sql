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
-- origin: an INSERT without a column list implies (a, b), which is not the other side's (b, a)
-- witness: $1 = 1, $2 = 2: A inserts (a, b) = (1, 2); B, given arrays [1] and [2] for (b, a), inserts
--   (a, b) = (2, 1)
CREATE TABLE w (a int, b int);
INSERT INTO w VALUES ($1, $2);
INSERT INTO w (b, a) SELECT * FROM unnest($1::int[], $2::int[]);
