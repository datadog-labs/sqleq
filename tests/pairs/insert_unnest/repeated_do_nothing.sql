-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqlsolver-rust: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: proved-gather
-- binding: gather
-- origin: a non-vacuous proof over repeated tuples: DO NOTHING keeps the first of two

-- The same tuple twice under DO NOTHING: the second row is skipped, the first is inserted, so the
-- VALUES side can succeed and the proof is not vacuous.
CREATE TABLE subs (email text PRIMARY KEY, plan text NOT NULL, since timestamptz DEFAULT now());
INSERT INTO subs (email, plan) VALUES ($1, $2), ($1, $2) ON CONFLICT (email) DO NOTHING;
INSERT INTO subs (email, plan) SELECT * FROM unnest($1::text[], $2::text[]) ON CONFLICT (email) DO NOTHING;
