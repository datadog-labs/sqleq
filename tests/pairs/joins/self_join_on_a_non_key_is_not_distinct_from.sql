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
-- catalog: inferred-seeded
-- origin: issue #241: the negative side. note is not a key, so the join pairs every two rows that share a note and a tag
-- witness: crate = {(1, 0, 0), (2, 0, 0)}: A returns two rows, and B four
CREATE TABLE crate (id int PRIMARY KEY, tag int, note int);
SELECT id, tag FROM crate;
SELECT a.id, b.tag FROM crate AS a JOIN crate AS b ON a.note = b.note AND a.tag IS NOT DISTINCT FROM b.tag;
