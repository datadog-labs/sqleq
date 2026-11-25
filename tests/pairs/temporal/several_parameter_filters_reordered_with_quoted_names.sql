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
-- catalog: inferred-seeded
-- origin: #301's review, the negative side: parameters bound to one row's values through quoted and qualified
--   names, and a range bound at its inclusive end only, refute nothing where the two statements only
--   write the same filters in another order
-- argument: AND is commutative, and NOT (created_at < $3) is created_at >= $3 on every row, a NULL
--   created_at included, where both are NULL and the row is dropped
CREATE TABLE t (id int PRIMARY KEY, tenant text, status text, kind text, created_at timestamp);
SELECT "x"."id" FROM "t" AS "x" WHERE "x"."tenant" = $1 AND "x"."status" = $2
  AND "x"."created_at" >= $3 AND "x"."created_at" < $4;
SELECT "id" FROM "t" WHERE "created_at" < $4 AND NOT ("created_at" < $3) AND "status" = $2 AND "tenant" = $1;
