-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: RANK() over one table (src/normalize.rs, lower_rank_windows): the rewrite writes the table's
--   own name, quoting and schema included, in its inner count, so the inner and outer references
--   name the one table
-- argument: with b not null, a row's rank is one more than the number of rows with a smaller b
create table s."T" (b integer not null);
SELECT b, RANK() OVER (ORDER BY b) AS r FROM s."T";
SELECT b, 1 + (SELECT COUNT(*) FROM s."T" AS w WHERE w.b < "T".b) AS r FROM s."T";
