-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: proved-gather-generated
-- binding: gather-generated
-- origin: a serial key the VALUES side leaves to DEFAULT and the unnest side supplies itself

-- The unnest side's id array holds the ids the DEFAULTs drew. That is the whole claim: as a
-- rewrite it bypasses the sequence, which the record's `generated.sequence` says.
CREATE TABLE orders (id serial PRIMARY KEY, customer text, total numeric);
INSERT INTO orders (id, customer, total) VALUES (DEFAULT, $1, $2), (DEFAULT, $3, $4);
INSERT INTO orders (id, customer, total) SELECT * FROM unnest($1::int[], $2::text[], $3::numeric[]);
