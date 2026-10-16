-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect lean: unsupported
-- origin: issue #168: rows drawn for a time column were strings Postgres rejects, so the table was
--   empty on every trial and no comparison over it could differ
-- witness: shifts = {(1, '12:00:00')}: B returns 1, A returns no row
CREATE TABLE shifts (id integer PRIMARY KEY, starts time NOT NULL);
SELECT id FROM shifts WHERE starts < '12:00:00';
SELECT id FROM shifts WHERE starts <= '12:00:00';
