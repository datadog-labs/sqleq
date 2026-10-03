-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: no-witness-generated
-- binding: gather-generated
-- origin: a possibly vacuous generated proof is not credited: DEFAULT on a NOT NULL column
-- argument: DEFAULT is NULL here, so B's array holds NULLs too, and both sides raise on every run

CREATE TABLE readings (sensor int NOT NULL, value numeric);
INSERT INTO readings (sensor, value) VALUES (DEFAULT, $1), (DEFAULT, $2);
INSERT INTO readings (sensor, value) SELECT * FROM unnest($1::int[], $2::numeric[]);
