-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqlsolver-rust: proved-literal
-- expect sqlsolver-jvm: proved-literal
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: the control beside the typmod fixes: a cast with no length must still be the identity
-- argument: $1 is compared with a varchar column either way, and a cast to unbounded varchar changes no value
create table "t" ("id" INTEGER, "k" VARCHAR, unique ("id"));
SELECT "id" FROM "t" WHERE "k" = $1::varchar;
SELECT "id" FROM "t" WHERE "k" = $1;
