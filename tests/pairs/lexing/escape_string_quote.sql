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
-- origin: issue #93 item 3: sqleq-fuzz took the `\'` in `E'a\';b'` for the end of the literal,
--   cut the pair file at the `;` after it, and ran pieces of the two queries as the pair
-- argument: `<>` is symmetric, and both sides compare `s` with the same text, `a';b`
create table "t" ("id" INTEGER PRIMARY KEY, "s" TEXT);
SELECT "id" FROM "t" WHERE "s" <> E'a\';b';
SELECT "id" FROM "t" WHERE E'a\';b' <> "s";
