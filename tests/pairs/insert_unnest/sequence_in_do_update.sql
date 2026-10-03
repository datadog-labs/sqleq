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
-- origin: the conflict clause reads the sequence the VALUES side's DEFAULT advances
-- witness: $1 = 'x', each side run twice: A's second run draws id 2, conflicts on name and sets
--   touched to 3; B, given ids [1] then [2], never advanced the sequence and sets touched to 1
CREATE SEQUENCE tag_seq;
CREATE TABLE tags (id bigint PRIMARY KEY DEFAULT nextval('tag_seq'), name text UNIQUE, touched bigint);
INSERT INTO tags (id, name) VALUES (DEFAULT, $1) ON CONFLICT (name) DO UPDATE SET touched = nextval('tag_seq');
INSERT INTO tags (id, name) SELECT * FROM unnest($1::bigint[], $2::text[]) ON CONFLICT (name) DO UPDATE SET touched = nextval('tag_seq');
