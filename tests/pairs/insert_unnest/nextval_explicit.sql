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
-- expect lean: proved-gather-generated
-- binding: gather-generated
-- origin: an explicit nextval in each VALUES row

CREATE SEQUENCE ticket_seq;
CREATE TABLE tickets (id bigint PRIMARY KEY, title text);
INSERT INTO tickets (id, title) VALUES (nextval('ticket_seq'), $1), (nextval('ticket_seq'), $2);
INSERT INTO tickets (id, title) SELECT * FROM unnest($1::bigint[], $2::text[]);
