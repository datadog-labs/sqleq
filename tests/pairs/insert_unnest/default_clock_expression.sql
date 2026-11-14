-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: proved-gather-generated
-- binding: gather-generated
-- origin: issue #215: a column default that is an expression over the statement clock was refused as an expression
--   around a function. It takes one value per statement, as now() does
CREATE TABLE ev2 (id int PRIMARY KEY, created_ms numeric DEFAULT (EXTRACT(epoch FROM CURRENT_TIMESTAMP) * 1000));
INSERT INTO ev2 (id, created_ms) VALUES ($1, DEFAULT), ($2, DEFAULT);
INSERT INTO ev2 (id, created_ms) SELECT * FROM unnest($1::int[], $2::numeric[]);
