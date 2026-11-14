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
-- expect lean: unsupported
-- binding: gather-generated
-- origin: issue #215: the negative side. clock_timestamp() differs between rows, so an expression over it is not
--   one value per statement, and stays refused
-- argument: under the binding the unnest side is given the values the VALUES side's DEFAULTs took, one per row,
--   and the two insert the same rows; the checker models one value per statement for a clock expression
CREATE TABLE ev2 (id int PRIMARY KEY, created_ms numeric DEFAULT (EXTRACT(epoch FROM clock_timestamp()) * 1000));
INSERT INTO ev2 (id, created_ms) VALUES ($1, DEFAULT), ($2, DEFAULT);
INSERT INTO ev2 (id, created_ms) SELECT * FROM unnest($1::int[], $2::numeric[]);
