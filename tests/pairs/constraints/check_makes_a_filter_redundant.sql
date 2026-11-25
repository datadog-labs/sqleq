-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: CHECK constraints reach the plan, as conditions no row meets, which sqleq-cvc5 asserts
-- argument: every row has `a > 0`: the column is NOT NULL and Postgres refuses a row the CHECK is false on
CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER NOT NULL CHECK (a > 0));
SELECT id FROM t WHERE a > 0;
SELECT id FROM t;
