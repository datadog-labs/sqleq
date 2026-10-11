-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: the Lean axis read a column declared `timestamp(3) with time zone` as `timestamp`,
--   dropping what follows the precision, so it took `timestamp[]` to be the column's own type and
--   proved this pair
-- witness: $1 = 1, $2 = '2020-01-01 00:00:00+05', with TimeZone UTC: A stores
--   2019-12-31 19:00:00+00; B reads the text as a timestamp, which drops the offset, and stores
--   2020-01-01 00:00:00+00

-- timestamp[] into a timestamptz column with a precision. The VALUES side's parameter is a
-- timestamptz; the unnest side's elements are timestamps, which the column then reads in the
-- session's time zone.
CREATE TABLE events (id int PRIMARY KEY, at timestamp(3) with time zone);
INSERT INTO events (id, at) VALUES ($1, $2), ($3, $4);
INSERT INTO events (id, at) SELECT * FROM unnest($1::int[], $2::timestamp[]);
