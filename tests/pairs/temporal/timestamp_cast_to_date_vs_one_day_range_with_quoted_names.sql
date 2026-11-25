-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #301's review: the row trials linked a parameter to its column by a pattern over the text that
--   reads no quoted name, so this pair, issue #296's with every name quoted as an ORM or pg_dump writes
--   it, got no row trial and no counterexample
-- witness: t = {(1, 'a', 'b', 'c', 'infinity')}, $1 = 'a', $2 = 'b', $3 = 'c', $4 = 'infinity': the cast of
--   created_at is infinity and A returns 1. 'infinity'::date + 1 is infinity, so B's range is empty and it
--   returns no row
CREATE TABLE t (id int PRIMARY KEY, tenant text, status text, kind text, created_at timestamp);
SELECT "x"."id" FROM "t" AS "x" WHERE "x"."tenant" = $1 AND "x"."status" = $2 AND "x"."kind" = $3
  AND "x"."created_at"::date = $4;
SELECT "id" FROM "t" WHERE "tenant" = $1 AND "status" = $2 AND "kind" = $3
  AND ("created_at" >= $4::date AND "created_at" < ($4::date + 1));
