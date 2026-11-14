-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- binding: gather-generated
-- origin: issue #215: the negative side. The DDL declares a function named after the generator, so a default that
--   calls it is that function, which may do anything, and stays refused
-- argument: this function only draws a random uuid, so under the binding the unnest side is given the ids the
--   VALUES side's DEFAULTs took and the two insert the same rows; the checker does not read a function's body
CREATE FUNCTION public.uuid_generate_v4() RETURNS uuid LANGUAGE sql AS 'SELECT gen_random_uuid()';
CREATE TABLE ev (id uuid PRIMARY KEY DEFAULT public.uuid_generate_v4(), name text);
INSERT INTO ev (id, name) VALUES (DEFAULT, $1), (DEFAULT, $2);
INSERT INTO ev (id, name) SELECT * FROM unnest($1::uuid[], $2::text[]);
