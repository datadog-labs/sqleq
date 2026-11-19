-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: model
-- expect lean: unsupported
-- origin: a guard for FOREIGN KEY constraints: under MATCH SIMPLE, one NULL referencing column
--   exempts the row
-- witness: p = {}, c = {(1, 1, NULL)}: A returns nothing, B returns 1
CREATE TABLE p (a INTEGER, b INTEGER, PRIMARY KEY (a, b));
CREATE TABLE c (id INTEGER PRIMARY KEY, a INTEGER NOT NULL, b INTEGER, FOREIGN KEY (a, b) REFERENCES p (a, b));
SELECT c.id FROM c WHERE EXISTS (SELECT 1 FROM p WHERE p.a = c.a);
SELECT id FROM c;
