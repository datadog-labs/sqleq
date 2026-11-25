-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: error
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #298's review: the types a parameter's uses state were compared by spelling, so a column declared
--   public.mood beside $1::mood was refused
-- argument: mood is public.mood, the one type of that name the DDL creates, so $1 has the column's type in both
--   statements and the cast is the identity
CREATE TYPE public.mood AS ENUM ('sad', 'ok');
CREATE TABLE t (id int PRIMARY KEY, m public.mood);
SELECT id FROM t WHERE m = $1::mood;
SELECT id FROM t WHERE m = $1;
