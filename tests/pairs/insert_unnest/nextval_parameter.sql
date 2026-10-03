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
-- expect lean: unsupported
-- binding: gather-generated
-- origin: the Lean axis's fragment boundary: a generator whose sequence is a parameter
-- argument: B's array $1 is column 1 of A's rows, the values nextval($1) drew, and B's $2 is
--   column 2, A's $2 and $3; so B inserts A's rows. Lean refuses the cell, soundly
CREATE SEQUENCE label_seq;
CREATE TABLE labels (id bigint PRIMARY KEY, name text);
INSERT INTO labels (id, name) VALUES (nextval($1), $2), (nextval($1), $3);
INSERT INTO labels (id, name) SELECT * FROM unnest($1::bigint[], $2::text[]);
