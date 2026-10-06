-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: nondet-skip
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect lean: unsupported
-- origin: issue #63: uuidv7() was missing from sqleq-fuzz's list of nondeterministic functions, so two runs of one query were compared
-- argument: the two sides are the same query
create table "t" ("id" INTEGER NOT NULL, unique ("id"));
SELECT "id", uuidv7() AS "x" FROM "t";
SELECT "id", uuidv7() AS "x" FROM "t";
