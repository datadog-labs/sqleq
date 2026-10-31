-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: issue #187's control: a literal and its neighbours drawn into a column must not refute an
--   exact rewrite of the boundary
-- argument: over integers, c < 334 holds exactly where c <= 333 does
CREATE TABLE t (c int4 NOT NULL);
SELECT * FROM t WHERE c < 334;
SELECT * FROM t WHERE c <= 333;
