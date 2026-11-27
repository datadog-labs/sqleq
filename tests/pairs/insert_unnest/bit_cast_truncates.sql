-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: #311's check of sqleq-lean: a bare bit is bit(1), and an explicit cast to it truncates. sqleq-lean read
--   the element type as the column's own and proved the pair
-- witness: $1 = 1, $2 = '10', $3 = 2, $4 = '1': A raises, since a bit(1) column does not take two bits; B
--   inserts (1, 1) and (2, 1)

-- An unnest element type of bit, with no length, is bit(1): the cast keeps one bit.
CREATE TABLE t (a int, b bit);
INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4);
INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::bit[]);
