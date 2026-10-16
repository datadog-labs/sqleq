-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: no-counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: proved-gather
-- binding: gather
-- origin: issue #168: values drawn for a time parameter were strings Postgres cannot read as a time
-- argument: B inserts the rows A's VALUES list spells, $2k+1 into id and $2k+2 into starts, one per
--   element of the two arrays
CREATE TABLE shifts (id integer, starts time NOT NULL);
INSERT INTO shifts (id, starts) VALUES ($1, $2), ($3, $4);
INSERT INTO shifts (id, starts) SELECT * FROM unnest($1::int4[], $2::time[]);
