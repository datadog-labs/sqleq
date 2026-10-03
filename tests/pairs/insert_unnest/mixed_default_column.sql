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
-- expect lean: proved-gather-generated
-- binding: gather-generated
-- origin: one column holding DEFAULT in some rows and a parameter in another

-- With a generated cell the parameters are numbered in reading order, skipping it.
CREATE TABLE notes (id serial PRIMARY KEY, body text);
INSERT INTO notes (id, body) VALUES (DEFAULT, $1), ($2, $3), (DEFAULT, $4);
INSERT INTO notes (id, body) SELECT * FROM unnest($1::int[], $2::text[]);
