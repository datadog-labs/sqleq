-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: the gather rule in sqleq-fuzz: a bare char is char(1), and an explicit cast to it
--   truncates
-- witness: $1 = 1, $2 = 'ab', $3 = 2, $4 = 'c': A inserts (1, ab) and (2, c); B inserts (1, a) and
--   (2, c)

-- An unnest element type of char, with no length, is char(1): the cast keeps one character.
CREATE TABLE t (a int, code text);
INSERT INTO t (a, code) VALUES ($1, $2), ($3, $4);
INSERT INTO t (a, code) SELECT * FROM unnest($1::int[], $2::char[]);
