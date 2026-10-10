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
-- origin: issue #142: the INSERT reduction compared the two statements' rows as written, and a
--   BEFORE INSERT trigger that numbers them from a sequence stores them by position
-- witness: t = {}, s fresh: A stores (1, 1) and (2, 2), B stores (1, 2) and (2, 1)
create sequence "s";
create table "t" ("id" INTEGER, "a" INTEGER);
create function "number_rows"() returns trigger language plpgsql as $$ begin NEW."id" := nextval('s'); return NEW; end $$;
create trigger "tr" before insert on "t" for each row execute function "number_rows"();
INSERT INTO t (id, a) VALUES (0, 1), (0, 2);
INSERT INTO t (id, a) VALUES (0, 2), (0, 1);
