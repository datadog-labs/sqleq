-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved !known-unsound
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: issue #234: sqleq-solver folded a product with a 0 factor to 0 whatever its type. numeric has NaN and
--   Infinity, and NaN * 0 and Infinity * 0 are NaN
-- witness: gauge = {(1, NULL, 'NaN')}: NaN * 0 is NaN, and NaN = 0 is false, so A returns no row and B returns 1
CREATE TABLE gauge (id int PRIMARY KEY, n int, amt numeric);
SELECT id FROM gauge WHERE amt * 0 = 0;
SELECT id FROM gauge WHERE amt IS NOT NULL;
