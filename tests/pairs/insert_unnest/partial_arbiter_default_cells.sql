-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: proved-gather-generated
-- binding: gather-generated
-- origin: issue #212: the partial-index arbiter with DEFAULT cells in the column its predicate tests. b has no
--   default, so each row holds NULL there and is one the index covers
-- argument: both sides name the same arbiter; given the values the DEFAULT cells took, here NULL, the unnest
--   side proposes the rows the VALUES side proposes, in the same order
CREATE TABLE w (a int, b text, c text);
CREATE UNIQUE INDEX w_a_live ON w (a) WHERE b IS NULL;
INSERT INTO w (a, b, c) VALUES ($1, DEFAULT, $2), ($3, DEFAULT, $4) ON CONFLICT (a) WHERE b IS NULL DO UPDATE SET c = excluded.c;
INSERT INTO w (a, b, c) SELECT * FROM unnest($1::int[], $2::text[], $3::text[]) ON CONFLICT (a) WHERE b IS NULL DO UPDATE SET c = excluded.c;
