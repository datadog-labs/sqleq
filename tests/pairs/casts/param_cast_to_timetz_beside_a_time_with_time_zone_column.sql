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
-- origin: #298's review: the types a parameter's uses state were compared by spelling, so a column declared time
--   with time zone beside $1::timetz was refused
-- argument: timetz is time with time zone, so $1 has the column's type in both statements and the cast is the
--   identity
CREATE TABLE t (id int PRIMARY KEY, z time with time zone);
SELECT id FROM t WHERE z = $1::timetz;
SELECT id FROM t WHERE z = $1;
