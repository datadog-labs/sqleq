-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: issue #167: where uuid-ossp cannot load, sqleq-fuzz dropped a DEFAULT
--   uuid_generate_v1mc(), so every INSERT that omits the column stored NULL in the key and failed
-- argument: both sides insert one row with kind $1, and the key's default draws a fresh uuid either way
create table "events" ("id" UUID PRIMARY KEY DEFAULT uuid_generate_v1mc(), "kind" TEXT NOT NULL);
INSERT INTO "events" ("kind") VALUES ($1);
INSERT INTO "events" ("kind") SELECT $1;
