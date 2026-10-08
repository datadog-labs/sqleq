-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: the Lean axis's fragment boundary: an explicit cast truncates where an assignment raises
-- witness: $1 = 1, $2 = 'abcdefghij': A raises (too long for varchar(8)); B inserts 'abcdefgh'

-- An explicit cast to varchar(8) truncates a long value, where the VALUES side's assignment raises.
CREATE TABLE t (a int, code varchar(8));
INSERT INTO t (a, code) VALUES ($1, $2);
INSERT INTO t (a, code) SELECT * FROM unnest($1::int[], $2::varchar(8)[]);
