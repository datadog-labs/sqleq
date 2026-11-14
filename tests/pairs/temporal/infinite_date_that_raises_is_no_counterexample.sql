-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: issue #210: the negative side of the infinite dates. A value that makes one statement raise is no
--   counterexample, and a trial that raises is not what the run reports
-- argument: d - d is 0 for every finite d, and for an infinite one Postgres raises 'cannot subtract infinite
--   dates', so wherever A runs without an error it selects exactly the rows whose d is not NULL
CREATE TABLE t (id int PRIMARY KEY, d date);
SELECT id FROM t WHERE d - d = 0;
SELECT id FROM t WHERE d IS NOT NULL;
