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
-- origin: a guard for CHECK constraints: an unnamed one has the name Postgres generates, which a
--   DROP CONSTRAINT may name
-- witness: t = {(1, 0)}: A returns nothing, B returns 1
CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER NOT NULL CHECK (a > 0));
ALTER TABLE t DROP CONSTRAINT t_a_check;
SELECT id FROM t WHERE a > 0;
SELECT id FROM t;
