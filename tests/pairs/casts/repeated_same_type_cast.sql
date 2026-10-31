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
-- origin: a second cast to the type a cast had just given ($2::uuid::uuid) was read through
--   q_exact_<type> while the first was dropped as the parameter's own type, so the pair lowered to
--   two plans
-- argument: a cast to the type its operand already has is the identity in Postgres, so the two
--   statements compare id with the same value
create table "t" ("id" UUID PRIMARY KEY, "a" INTEGER);
UPDATE "t" SET "a" = $1 WHERE "id" = $2::uuid::UUID;
UPDATE "t" SET "a" = $1 WHERE "id" = $2::uuid;
