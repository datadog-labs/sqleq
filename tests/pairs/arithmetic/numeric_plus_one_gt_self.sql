-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: proved !known-unsound
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: issue #234: a prover that reads numeric as the real numbers proves x + 1 > x for every x that is not NULL.
--   Infinity + 1 is Infinity and NaN + 1 is NaN, and neither is greater than itself
-- witness: gauge = {(1, NULL, 'Infinity')}: Infinity + 1 > Infinity is false, so A returns no row and B returns 1
CREATE TABLE gauge (id int PRIMARY KEY, n int, amt numeric);
SELECT id FROM gauge WHERE amt + 1 > amt;
SELECT id FROM gauge WHERE amt IS NOT NULL;
