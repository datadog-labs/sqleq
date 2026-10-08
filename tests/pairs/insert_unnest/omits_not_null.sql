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
-- expect lean: no-witness
-- binding: gather
-- origin: a proof that may be vacuous is not credited: neither side supplies a NOT NULL column
-- argument: under the gather rule the array has one element per VALUES row, so both sides insert
--   two rows with a NULL owner and raise on every run

-- `owner` is NOT NULL with no default and neither side supplies it: every run fails.
CREATE TABLE docs (id bigserial PRIMARY KEY, owner int NOT NULL, title text);
INSERT INTO docs (title) VALUES ($1), ($2);
INSERT INTO docs (title) SELECT * FROM unnest($1::text[]);
