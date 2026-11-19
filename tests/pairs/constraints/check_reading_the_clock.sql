-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: a guard for CHECK constraints: Postgres checks a row when it is written, so one that
--   reads the clock may no longer hold at the query
-- witness: a row (1, 2026-01-01) written on 2026-01-01, read on 2026-01-02: A returns nothing, B
--   returns 1
CREATE TABLE t (id INTEGER PRIMARY KEY, d DATE NOT NULL CHECK (d >= CURRENT_DATE));
SELECT id FROM t WHERE d >= CURRENT_DATE;
SELECT id FROM t;
