-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #63: sqleq-fuzz substituted a parameter value for `$1` inside a string literal
-- argument: '$1' is a two-character string literal, not a placeholder, and '$' || '1' is the same string
create table "t" ("id" INTEGER, unique ("id"));
SELECT '$1' AS "x" FROM "t";
SELECT '$' || '1' AS "x" FROM "t";
