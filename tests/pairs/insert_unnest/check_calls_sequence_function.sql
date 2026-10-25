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
-- expect cvc5: no-plan
-- expect lean: unsupported
-- binding: gather-generated
-- origin: a CHECK calls a function that reads the sequence the generated cells draw from (#178)
-- witness: $1 = 'x', $2 = 'y': A inserts both rows; B, given ids [1, 2], fails with "currval of
--   sequence "item_seq" is not yet defined in this session"
CREATE SEQUENCE item_seq;
CREATE FUNCTION seq_at(bigint) RETURNS boolean LANGUAGE sql AS 'SELECT currval(''item_seq'') = $1';
CREATE TABLE items (id bigint PRIMARY KEY CHECK (seq_at(id)), name text);
INSERT INTO items (id, name) VALUES (nextval('item_seq'), $1), (nextval('item_seq'), $2);
INSERT INTO items (id, name) SELECT * FROM unnest($1::bigint[], $2::text[]);
