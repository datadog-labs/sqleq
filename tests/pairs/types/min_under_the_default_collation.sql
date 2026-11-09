-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-schema
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: PR #209 review: the cvc5 axis ordered strings by code point for MIN, MAX and row slices, which
--   carry no collation and are in the database's default one, which no input states; it proved
--   this pair, which en_US.utf8 contradicts. The order is uninterpreted there now
--   (sqleq-cvc5/src/translate.rs, less)
-- witness: in a database created with LC_COLLATE 'en_US.utf8': A returns 'a', B returns 'B' (in a C
--   database both return 'B')
SELECT MIN(x) AS m FROM (VALUES ('a'), ('B')) AS v(x);
SELECT 'B' AS m;
