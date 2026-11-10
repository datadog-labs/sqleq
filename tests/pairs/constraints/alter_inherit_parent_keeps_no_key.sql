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
-- origin: `ALTER TABLE ... INHERIT`, which sqlparser reads from 0.64 on (#264). A scan of the parent
--   then reads the child's rows too, so the parent's key does not hold over what it reads. Raw DDL's
--   rejected statements were always read so; a statement the parser reads but the catalog has no arm
--   for is now read the same way, and without that both qed and sqleq-solver proved this pair
-- witness: p = {(1)}, c = {(1)}: A returns 1 once, B returns 1 twice
CREATE TABLE p (k INTEGER PRIMARY KEY);
CREATE TABLE c (k INTEGER NOT NULL);
ALTER TABLE c INHERIT p;
SELECT DISTINCT k FROM p;
SELECT k FROM p;
