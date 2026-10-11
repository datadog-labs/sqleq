-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: the gather rule in sqleq-fuzz: timestamptz elements into a timestamp column move with
--   the session's time zone
-- witness: TimeZone America/New_York, $1 = 1, $2 = '2021-03-14 02:30:00': A inserts 02:30; B
--   inserts 03:30, as 02:30 does not exist on that day in New York

-- Read as a timestamptz and assigned back to a timestamp, a local time can change: where the
-- clocks move forward, a time inside the gap comes back an hour later.
CREATE TABLE t (a int, at timestamp);
INSERT INTO t (a, at) VALUES ($1, $2);
INSERT INTO t (a, at) SELECT * FROM unnest($1::int[], $2::timestamptz[]);
