-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: issue #93 item 3: sqleq-fuzz cut a pair file at the `;` inside a nested block comment,
--   which Postgres ends only at its second `*/`, and found three queries; the `;` in the function's
--   dollar-quoted body ends no statement either
-- argument: `"a" > 1` and `1 < "a"` are one comparison
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
create function "inc"("x" INTEGER) returns INTEGER language sql immutable as $$ SELECT "x" + 1; $$;
SELECT "id" FROM "t" WHERE "a" > 1 /* outer /* inner */ still a comment; */;
SELECT "id" FROM "t" WHERE 1 < "a";
