-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #210: sqleq-fuzz's date and timestamp domains had no infinity, so a cast to date was never told
--   from the one-day range that agrees with it on every finite value
-- witness: t = {(1, 'infinity')}, $1 = 'infinity': A returns 1; in B 'infinity'::date + 1 is 'infinity', so the
--   range is empty and B returns no row
CREATE TABLE t (id int PRIMARY KEY, ts timestamp);
SELECT id FROM t WHERE ts::date = $1;
SELECT id FROM t WHERE ts >= $1::date AND ts < ($1::date + 1);
