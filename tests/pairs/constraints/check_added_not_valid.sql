-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: model
-- expect lean: unsupported
-- origin: a guard for CHECK constraints: one added NOT VALID holds only of the rows written after it
-- witness: t = {(1, 0)} written before the ALTER TABLE: A returns nothing, B returns 1
CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER NOT NULL);
ALTER TABLE t ADD CONSTRAINT positive CHECK (a > 0) NOT VALID;
SELECT id FROM t WHERE a > 0;
SELECT id FROM t;
