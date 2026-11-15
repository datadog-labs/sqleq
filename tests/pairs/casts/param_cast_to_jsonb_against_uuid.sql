-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #285: a cast over a bare parameter to a type inference has no class for was dropped whatever the
--   type, so ($1)::jsonb and ($1)::uuid were one node
-- witness: t = {(1)}, $1 = '12345678901234567890123456789012': A returns that JSON number and B the uuid
--   12345678-9012-3456-7890-123456789012
CREATE TABLE t (id int PRIMARY KEY);
SELECT id, ($1)::jsonb AS x FROM t;
SELECT id, ($1)::uuid AS x FROM t;
