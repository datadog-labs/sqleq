-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect cvc5: unsupported
-- expect lean: unsupported
-- origin: PR #209 review: the cvc5 axis added OFFSET and LIMIT with wrapping arithmetic, so this slice was
--   empty to it and proved equal to WHERE false; a bound that overflows is refused
--   (sqleq-cvc5/src/translate.rs)
-- witness: t = {(1), (2)}: A returns 2, B returns no rows
create table t (k integer primary key);
SELECT k FROM t ORDER BY k LIMIT 9223372036854775807 OFFSET 1;
SELECT k FROM t WHERE k < 0 AND k > 0;
