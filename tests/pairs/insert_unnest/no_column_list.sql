-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect cvc5: no-plan
-- expect lean: proved-gather
-- binding: gather
-- origin: an INSERT without a column list targets the table's columns in declared order, so Lean reads
--   the list from the DDL
CREATE TABLE w (a text, b int);
INSERT INTO w VALUES ($1, $2), ($3, $4);
INSERT INTO w SELECT * FROM unnest($1::text[], $2::int[]);
