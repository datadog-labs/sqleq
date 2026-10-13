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
-- expect lean: unsupported
-- binding: gather
-- origin: an INSERT clause sqlparser reads only from apache/datafusion-sqlparser-rs#2626; Lean's
--   fragment does not model it, so it refuses it by name
-- argument: both sides carry the same OVERRIDING SYSTEM VALUE, so the identity column stores the
--   supplied id on both; under the gather rule the unnest side inserts the VALUES side's rows, the
--   same rows in the same order

CREATE TABLE t (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, a text);
INSERT INTO t (id, a) OVERRIDING SYSTEM VALUE VALUES ($1, $2), ($3, $4);
INSERT INTO t (id, a) OVERRIDING SYSTEM VALUE SELECT * FROM unnest($1::int[], $2::text[]);
