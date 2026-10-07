-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #108: box(point, point) returns a box, whose = is not transitive, and the result of a function nobody declared was read as a type whose = is an equivalence
-- witness: t = {(1, '(0,0)', '(1,1)', '(1,1.0000009)', '(1,1.0000018)')}: the three boxes have areas 1, 1.0000009 and 1.0000018, so the first two equalities hold and the third does not; A yields 1, B yields no row
create table "t" ("id" INTEGER, "o" point, "p" point, "q" point, "r" point);
SELECT "id" FROM "t" WHERE box("o", "p") = box("o", "q") AND box("o", "q") = box("o", "r");
SELECT "id" FROM "t" WHERE box("o", "p") = box("o", "q") AND box("o", "q") = box("o", "r") AND box("o", "p") = box("o", "r");
