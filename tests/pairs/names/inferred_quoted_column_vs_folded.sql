-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-schema
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- catalog: inferred
-- origin: issue #112: once type inference keeps a quoted name's case, "A" and a are two
--   synthesized columns of t, as they are two columns in Postgres; they must not become one
-- witness: t ("A", a) = {(1, 2)}: A returns 1, B returns 2
SELECT "A" FROM "t";
SELECT a FROM "t";
