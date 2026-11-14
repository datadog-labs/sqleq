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
-- expect lean: proved-gather-generated
-- binding: gather-generated
-- origin: issue #215: a column default calling a generator under a schema, public.uuid_generate_v4(), was refused as
--   a function of a user's. The DDL declares no function of that name, so it is uuid-ossp's, as the bare name is
CREATE TABLE ev (id uuid PRIMARY KEY DEFAULT public.uuid_generate_v4(), name text);
INSERT INTO ev (id, name) VALUES (DEFAULT, $1), (DEFAULT, $2);
INSERT INTO ev (id, name) SELECT * FROM unnest($1::uuid[], $2::text[]);
