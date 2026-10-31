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
-- origin: PR #209 review: with '00' and '0' kept as strings, a prover could tell COALESCE(i, '00') from
--   COALESCE(i, '0') where i is NULL, which Postgres cannot: both are the integer 0
-- witness: t3 = {(NULL)}: A returns true (0 = 0), B returns false
create table t3 (i integer);
SELECT COALESCE(i, '00') = COALESCE(i, '0') AS b FROM t3;
SELECT i IS NOT NULL AS b FROM t3;
