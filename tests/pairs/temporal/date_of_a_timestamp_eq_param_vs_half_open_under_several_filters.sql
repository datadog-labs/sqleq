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
-- origin: #301's review: $4 meets created_at only under a cast, on both sides, and a parameter was bound from
--   a row only through a comparison with the bare column. Postgres makes $4 a date, which reads the
--   timestamp the row holds, so it is bound with the other three
-- witness: t = {(1, 'a', 'b', 'c', 'infinity')}, $1 = 'a', $2 = 'b', $3 = 'c', $4 = 'infinity': the cast of
--   created_at is infinity and A returns 1. 'infinity'::date + 1 is infinity, which no date is below, so B
--   returns no row
CREATE TABLE t (id int PRIMARY KEY, tenant text, status text, kind text, created_at timestamp);
SELECT id FROM t WHERE tenant = $1 AND status = $2 AND kind = $3 AND created_at::date = $4;
SELECT id FROM t WHERE tenant = $1 AND status = $2 AND kind = $3
  AND created_at::date >= $4 AND created_at::date < $4 + 1;
