-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: PR #209 review, split: a bare NULL branch voted for the type of a CASE as an INTEGER would, so
--   CASE WHEN a > 1 THEN TRUE ELSE NULL END was an INTEGER beside a BOOLEAN, and no prover could
--   relate the two sides; a bare NULL has no type of its own and takes the other branches'
--   (src/types.rs, make_case, is_bare_null)
-- argument: the CASE is TRUE where a > 1 and NULL elsewhere, and so is (a > 1) OR NULL: TRUE OR NULL is
--   TRUE, FALSE OR NULL and NULL OR NULL are NULL
create table t (a integer);
SELECT CASE WHEN a > 1 THEN TRUE ELSE NULL END AS b FROM t;
SELECT (a > 1) OR NULL AS b FROM t;
