-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: PR #209 review: canonical_aliases never renamed a whole-row reference, so renaming the declarations
--   made two queries Postgres tells apart one tree, credited reflexive; a query whose text still names
--   an alias after the renaming is now left as it was (src/normalize.rs)
-- witness: emp = {(1, 1)}, dept = {(1)}: A returns {"id":1,"dept_id":1}, B returns {"id":1}
create table emp (id integer, dept_id integer);
create table dept (id integer);
SELECT row_to_json(e) AS j FROM emp AS e JOIN dept AS d ON e.dept_id = d.id;
SELECT row_to_json(e) AS j FROM emp AS d JOIN dept AS e ON d.dept_id = e.id;
