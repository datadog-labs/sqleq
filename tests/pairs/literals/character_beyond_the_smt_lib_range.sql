-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: unsupported
-- expect lean: unsupported
-- origin: PR #209 review: the cvc5 axis wrote a character above U+2FFFF as the escape \u{30000}, which is
--   outside SMT-LIB's string range and which cvc5 keeps as that text, the same string as the
--   nine-character literal on side B; such a character is refused (sqleq-cvc5/src/translate.rs)
-- witness: t = {('𰀀')}: A returns the row, B returns none
create table t (s varchar);
SELECT s FROM t WHERE s = '𰀀';
SELECT s FROM t WHERE s = '\u{30000}';
