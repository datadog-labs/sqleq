-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: nondet-skip
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect cvc5: no-plan
-- expect lean: proved-gather-generated
-- binding: gather-generated
-- origin: a generator whose sequence is a parameter, one per row, numbered in reading order
CREATE SEQUENCE label_seq;
CREATE TABLE labels (id bigint PRIMARY KEY, name text);
INSERT INTO labels (id, name) VALUES (nextval($1), $2), (nextval($3), $4);
INSERT INTO labels (id, name) SELECT * FROM unnest($1::bigint[], $2::text[]);
