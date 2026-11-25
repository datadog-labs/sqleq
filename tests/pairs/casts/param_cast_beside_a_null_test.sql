-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #298's review: $1 IS NULL was counted as a use that states no type, which refused the optional-filter
--   idiom beside a cast to a type inference has no class for
-- argument: IS NULL types nothing, so $1 is a uuid in both statements: from the comparison with u in B, and from the
--   cast in A, where the cast is then the identity
CREATE TABLE t (id int PRIMARY KEY, u uuid);
SELECT id FROM t WHERE (u = $1::uuid OR $1 IS NULL);
SELECT id FROM t WHERE (u = $1 OR $1 IS NULL);
