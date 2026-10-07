-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: issue #48: sqleq-lean compared the INSERT targets by their last name only, so a.events and
--   b.events passed as one table
-- witness: schemas a and b each hold an empty events (id bigint NOT NULL, kind text); VALUES side
--   $1 = 1, $2 = 'x', $3 = 2, $4 = 'y', unnest side $1 = {1,2}, $2 = {x,y}: A fills a.events and
--   leaves b.events empty, B leaves a.events empty and fills b.events

CREATE TABLE a.events (id bigint NOT NULL, kind text);
INSERT INTO a.events (id, kind) VALUES ($1, $2), ($3, $4);
INSERT INTO b.events (id, kind) SELECT * FROM unnest($1::bigint[], $2::text[]);
