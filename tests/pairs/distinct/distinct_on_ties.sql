-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect lean: unsupported
-- origin: issue #89: sqleq-fuzz compared the row DuckDB kept among rows tied inside a DISTINCT ON key as
--   a bag, though Postgres chooses it by physical order
-- argument: B's inner ORDER BY does not constrain the outer sort, so both sides keep one arbitrary row
--   per g, which docs/SOUNDNESS.md ("A row slice is taken as deterministic") reads as the same choice.
--   On t = {(2, NULL, 2), (1, 2, 0), (0, 2, 1)} Postgres 17 keeps v = 0 for g = 2 in A and v = 1 in B,
--   and v = 1 in A once the same rows are inserted in reverse order

create table "t" ("id" INTEGER, "g" INTEGER, "v" INTEGER, unique ("id"));
SELECT DISTINCT ON ("g") "g", "v" FROM "t" ORDER BY "g";
SELECT DISTINCT ON ("g") "g", "v" FROM (SELECT * FROM "t" ORDER BY "v" DESC) AS "s" ORDER BY "g";
