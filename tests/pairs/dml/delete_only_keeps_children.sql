-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: `DELETE FROM ONLY p`, whose marker sqlparser reads from 0.64 on (#264); the DML reduction
--   reads only the target's name, so the marker is refused before it, wherever a table is named
-- witness: p = {(1, 10)}, its child c = {(1, 20)}: A leaves c's row, B deletes it
CREATE TABLE p (k INTEGER, a INTEGER);
CREATE TABLE c () INHERITS (p);
DELETE FROM ONLY p WHERE k = 1;
DELETE FROM p WHERE k = 1;
