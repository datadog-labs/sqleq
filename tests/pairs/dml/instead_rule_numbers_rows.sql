-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: `CREATE RULE`, which sqlparser reads from 0.64 on (#264); a rule on an event other than
--   SELECT is a trigger on its table, so no DML on it is reduced, as when the parser could not read
--   the rule and the catalog read it off its words
-- witness: log = {}, s fresh: A stores (1, 1) and (2, 2) in log, B stores (1, 2) and (2, 1)
create sequence "s";
create table "t" ("id" INTEGER, "a" INTEGER);
create table "log" ("n" INTEGER, "a" INTEGER);
create rule "r" as on insert to "t" do instead insert into "log" ("n", "a") values (nextval('s'), NEW."a");
INSERT INTO t (id, a) VALUES (0, 1), (0, 2);
INSERT INTO t (id, a) VALUES (0, 2), (0, 1);
