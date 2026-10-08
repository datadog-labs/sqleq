-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-schema
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- catalog: inferred
-- origin: issue #112: type inference lower-cased the quoted schema "S", so "S".t and s.t were
--   one synthesized table, though strip_schema keeps them apart
-- witness: "S".t = {(1)}, s.t = {(2)}: A returns 1, B returns 2
SELECT "a" FROM "S"."t";
SELECT "a" FROM "s"."t";
