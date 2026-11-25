-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #285, its second pair: the array cast dropped in a select list
-- witness: t = {(1, 1, 'a')}: A returns the bigint[] {7} for $1 = '{7}' and B the bigint 7 for $1 = '7'. No one
--   text is read by both: '7' is no array and '{7}' is no bigint
CREATE TABLE t (id int PRIMARY KEY, n bigint, name varchar(20));
SELECT id, ($1)::bigint[] AS x FROM t;
SELECT id, ($1)::bigint AS x FROM t;
