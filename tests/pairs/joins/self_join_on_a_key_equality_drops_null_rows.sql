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
-- origin: issue #241: the negative side. = in place of IS NOT DISTINCT FROM is UNKNOWN on a NULL tag, and the row goes
-- witness: crate = {(1, NULL, NULL)}: A returns the row, and B none, since NULL = NULL is not TRUE
CREATE TABLE crate (id int PRIMARY KEY, tag int, note int);
SELECT id, tag FROM crate;
SELECT a.id, b.tag FROM crate AS a JOIN crate AS b ON a.id = b.id AND a.tag = b.tag;
