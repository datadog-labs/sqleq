-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: model
-- expect lean: unsupported
-- origin: a guard for CHECK constraints: Postgres takes a row the CHECK is NULL on, so the plan's
--   condition is the rows it is false on, not those it is not true on
-- witness: t = {(1, NULL)}: A returns nothing, B returns 1
CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER CHECK (a > 0));
SELECT id FROM t WHERE a > 0;
SELECT id FROM t;
