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
-- origin: a table CHECK over two columns, added by ALTER TABLE, reaches the plan
-- argument: every row has `lo <= hi`: both columns are NOT NULL and the CHECK refuses the rest
CREATE TABLE t (id INTEGER PRIMARY KEY, lo INTEGER NOT NULL, hi INTEGER NOT NULL);
ALTER TABLE t ADD CONSTRAINT ordered CHECK (lo <= hi);
SELECT id FROM t WHERE lo <= hi;
SELECT id FROM t;
