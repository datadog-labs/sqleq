-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect lean: unsupported
-- origin: issue #187: a date literal outside the generated dates hid a boundary difference
-- witness: t = {('2000-01-01')}: B returns it, A returns no row
CREATE TABLE t (c1 date);
SELECT * FROM t WHERE c1 > '2000-01-01'::date;
SELECT * FROM t WHERE c1 >= '2000-01-01'::date;
