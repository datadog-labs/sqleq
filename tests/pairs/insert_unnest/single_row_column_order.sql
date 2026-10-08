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
-- origin: one row, two DEFAULT cells, and a column list out of the table's order

CREATE TABLE logins (id serial PRIMARY KEY, seen timestamptz DEFAULT now(), username text);
INSERT INTO logins (username, seen, id) VALUES ($1, DEFAULT, DEFAULT);
INSERT INTO logins (username, seen, id) SELECT * FROM unnest($1::text[], $2::timestamptz[], $3::int[]);
