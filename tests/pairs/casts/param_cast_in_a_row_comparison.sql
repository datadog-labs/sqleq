-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #302's review, measured on a corpus: a row comparison is a use the rule has to read, field by field, or
--   a pair that writes its filter as one loses its proof to the refusal
-- argument: (d, a) = ($1, $2) is d = $1 AND a = $2, so $1 is a date and $2 an integer in both statements and each
--   cast is the identity
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT id FROM t WHERE (d, a) = ($1::date, $2::int);
SELECT id FROM t WHERE (d, a) = ($1, $2);
