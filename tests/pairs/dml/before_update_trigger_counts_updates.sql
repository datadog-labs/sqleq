-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #142: the UPDATE reduction compared the values the two statements write, and a
--   BEFORE UPDATE trigger counts the rows an UPDATE touches, whether or not it changes them
-- witness: t = {(1, 1, 0)}: A touches the row and leaves (1, 1, 1), B does not and leaves (1, 1, 0)
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER, "updates" INTEGER NOT NULL DEFAULT 0);
create function "count_updates"() returns trigger language plpgsql as $$ begin NEW."updates" := OLD."updates" + 1; return NEW; end $$;
create trigger "tr" before update on "t" for each row execute function "count_updates"();
UPDATE "t" SET "a" = 1 WHERE "id" = 1;
UPDATE "t" SET "a" = 1 WHERE "id" = 1 AND "a" IS DISTINCT FROM 1;
