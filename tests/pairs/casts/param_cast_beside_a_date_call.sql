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
-- origin: #302's review, measured on a corpus: date(x) is a cast to date written as a call, and a comparison with
--   it is the one comparison with a call the rule reads
-- argument: date(ts) is a date whatever ts is, so $1 is a date in both statements and the cast is the identity
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT id FROM t WHERE date(ts) = $1::date;
SELECT id FROM t WHERE date(ts) = $1;
