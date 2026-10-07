-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-tables
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: WITH ORDINALITY (and with it every table function in FROM) is refused, not lowered
--   (docs/SOUNDNESS.md); a pin for each documented refusal (#68)
-- witness: any instance: A returns (5, 1) and (7, 2), B returns (5, 5) and (7, 7)
create table "t" ("id" INTEGER);
SELECT z.x, z.n FROM unnest(ARRAY[5, 7]) WITH ORDINALITY AS z(x, n);
SELECT z.x, z.x FROM unnest(ARRAY[5, 7]) WITH ORDINALITY AS z(x, n);
