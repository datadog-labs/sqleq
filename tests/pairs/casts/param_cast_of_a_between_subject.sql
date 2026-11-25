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
-- origin: #302's review, the positive side: the subject of a BETWEEN whose bounds are of one type is a use the
--   rule reads
-- argument: $1 BETWEEN lo AND hi is $1 >= lo AND $1 <= hi over two timestamps, so $1 is a timestamp in both
--   statements and the cast is the identity
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT id FROM t WHERE $1::timestamp BETWEEN lo AND hi;
SELECT id FROM t WHERE $1 BETWEEN lo AND hi;
