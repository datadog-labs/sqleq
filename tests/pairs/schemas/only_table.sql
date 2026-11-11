-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: `FROM ONLY t`, which sqlparser reads from 0.64 on (#264); `t` has no children here, so
--   `ONLY` excludes nothing, but the frontend models no inheritance and refuses the marker rather
--   than read `ONLY t` as `t`
-- argument: `ONLY` leaves out the tables that inherit from `t`, and no table does in this DDL
CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER);
SELECT a FROM ONLY t;
SELECT a FROM t;
