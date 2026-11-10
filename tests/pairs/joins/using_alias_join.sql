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
-- origin: `JOIN ... USING (k) AS j`, which sqlparser reads from 0.64 on (#264); the alias names the
--   merged column and changes nothing a query reads, but the frontend does not lower the form and
--   refuses it, as it refused the text the parser could not read
-- argument: the alias only names the merged columns of `USING (k)`; the join and its rows are the same
CREATE TABLE a (k INTEGER, x INTEGER);
CREATE TABLE b (k INTEGER, y INTEGER);
SELECT a.x FROM a JOIN b USING (k) AS j;
SELECT a.x FROM a JOIN b USING (k);
