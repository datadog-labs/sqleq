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
-- origin: a domain's DEFAULT was not read, so a column of a domain whose default is a sequence was
--   taken for row-determined by the INSERT reduction
-- witness: t = {}, the sequence fresh: A adds (1, 1) and (2, 2), B adds (1, 2) and (2, 1)
create sequence "s";
create domain "d" as INTEGER default nextval('s');
create table "t" ("id" "d", "a" INTEGER);
INSERT INTO t (a) VALUES (1), (2);
INSERT INTO t (a) VALUES (2), (1);
