-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- origin: a USING after a RIGHT join compared the first binding of the name, not the column the
--   join merged (#31)
-- witness: u = {}, v = w = {(1)}: the merged a is COALESCE(u.a, v.a) = 1, so A joins w and returns
--   a row; u.a is NULL, so B returns none
CREATE TABLE u (a INTEGER);
CREATE TABLE v (a INTEGER);
CREATE TABLE w (a INTEGER);
SELECT 1 FROM u RIGHT JOIN v USING (a) JOIN w USING (a);
SELECT 1 FROM u RIGHT JOIN v USING (a) JOIN w ON u.a = w.a;
