-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #111: type inference also synthesized a catalog of its own, which the seeded mode
--   then discarded, and refused the pair because t is read only through count(*), so no column of
--   it could be synthesized. The declared catalog refuses a bare $1, so no catalog lowered the pair
-- argument: an inner join commutes, and the filter reads u alone, so both sides count the rows of
--   t x u with u.a = $1
create table "t" ("a" INTEGER, "b" INTEGER);
create table "u" ("a" INTEGER, "b" INTEGER);
SELECT count(*) FROM "t", "u" WHERE "u"."a" = $1;
SELECT count(*) FROM "u", "t" WHERE "u"."a" = $1;
