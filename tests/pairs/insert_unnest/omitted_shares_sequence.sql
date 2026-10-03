-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: nondet-skip
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- binding: gather-generated
-- origin: an omitted column's default draws from the sequence the generated cells use
-- witness: $1 = 'x', $2 = 'y': A interleaves the draws and inserts (1, 2, x) and (3, 4, y); B, given
--   ids [1, 3], draws only for pos and inserts (1, 1, x) and (3, 2, y)
CREATE SEQUENCE item_seq;
CREATE TABLE items (id bigint PRIMARY KEY, pos bigint DEFAULT nextval('item_seq'), name text);
INSERT INTO items (id, name) VALUES (nextval('item_seq'), $1), (nextval('item_seq'), $2);
INSERT INTO items (id, name) SELECT * FROM unnest($1::bigint[], $2::text[]);
