-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: PR #209: the frontend's rewrites write identifiers beginning __sqleq_ (the aliases
--   canonical_aliases gives), so an identifier of the input spelled so is refused before any rewrite
--   runs and can never be taken for one of them; such a pair is never credited
-- argument: an output column's name does not change the rows
create table t (a integer);
SELECT 1 AS "__sqleq_x" FROM t;
SELECT 1 AS "x" FROM t;
