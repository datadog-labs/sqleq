-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: #300's review: an unquoted current_role in the target's predicate was read as the column of that
--   name, the index was taken as the arbiter, and the pair was credited with a witness that does not exist
-- argument: the unquoted current_role is the function, not the column "current_role", so the target's
--   predicate does not imply the index's and both sides raise before reading a row (there is no unique or
--   exclusion constraint matching the ON CONFLICT specification)
CREATE TABLE w (a int, "current_role" text, c text);
CREATE UNIQUE INDEX w_a_live ON w (a) WHERE "current_role" IS NULL;
INSERT INTO w (a, c) VALUES ($1, $2), ($3, $4) ON CONFLICT (a) WHERE current_role IS NULL DO UPDATE SET c = excluded.c;
INSERT INTO w (a, c) SELECT * FROM unnest($1::int[], $2::text[]) ON CONFLICT (a) WHERE current_role IS NULL DO UPDATE SET c = excluded.c;
