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
-- origin: issue #241: the split in its barest form. tag IS NOT DISTINCT FROM tag is [tag is NULL] or [tag is not NULL],
--   which holds of every row
-- argument: a value is not distinct from itself, NULL included, so the filter keeps every row
CREATE TABLE crate (id int PRIMARY KEY, tag int, note int);
SELECT id FROM crate;
SELECT id FROM crate WHERE tag IS NOT DISTINCT FROM tag;
