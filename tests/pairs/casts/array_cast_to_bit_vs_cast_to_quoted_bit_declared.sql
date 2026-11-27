-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: #311, under the declared catalog: both casts were the function q_cast_bit_array, whose name writes a
--   quote as it writes a space
-- witness: t = {(1, '{1011}', '{1011}')}: A compares bs with {1} and returns no row. B compares it with {1011} and
--   returns 1
CREATE TABLE t (id int PRIMARY KEY, bs bit(4)[], cs bit(4)[]);
SELECT id FROM t WHERE bs = cs::bit[];
SELECT id FROM t WHERE bs = cs::"bit"[];
