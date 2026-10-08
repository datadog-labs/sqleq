-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: nondet-skip
-- expect qed: proved
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #124: sqleq-fuzz compared a trial whose LIMIT leaves ties by cardinality alone even under a
--   filter, where the tied row the LIMIT keeps decides how many rows survive
-- argument: "id" is a key, so DISTINCT keeps every row of "t" and both sides take one arbitrary row among
--   those with the smallest g, read as the same choice (docs/SOUNDNESS.md, "A row slice is taken as
--   deterministic"). Postgres reads them in different orders: on t = {(2, NULL, 1), (0, NULL, 2)}, inserted
--   in that order, Postgres 17 keeps (2, NULL, 1) in A, the first row inserted, and returns id 2, but hashes
--   the rows for DISTINCT, keeps (0, NULL, 2) in B and returns no row. Inserted in reverse order, A returns
--   no row too
-- sqleq-fuzz refuted the pair on that instance, an alarm against the provers' proof. A cut whose order
-- leaves ties is now compared by cardinality only where no level above it reads its rows to decide which
-- rows it returns, and here a WHERE does.

create table "t" ("id" INTEGER PRIMARY KEY, "g" INTEGER, "v" INTEGER);
SELECT "id" FROM (SELECT * FROM "t" ORDER BY "g" LIMIT 1) AS "s" WHERE "v" = 1;
SELECT "id" FROM (SELECT DISTINCT * FROM "t" ORDER BY "g" LIMIT 1) AS "s" WHERE "v" = 1;
