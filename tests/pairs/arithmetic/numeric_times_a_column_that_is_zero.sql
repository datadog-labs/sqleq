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
-- origin: issue #234: the 0 need not be written in the product. sqleq-solver put the 0 that n = 0 gives in place of n,
--   and then folded amt * 0 to 0
-- witness: gauge = {(1, 0, 'NaN')}: NaN * 0 is NaN, so A returns no row and B returns 1
CREATE TABLE gauge (id int PRIMARY KEY, n int, amt numeric);
SELECT id FROM gauge WHERE n = 0 AND amt * n = 0;
SELECT id FROM gauge WHERE n = 0 AND amt IS NOT NULL;
