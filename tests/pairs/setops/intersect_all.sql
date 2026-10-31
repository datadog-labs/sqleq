-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: unsupported
-- expect cvc5: model
-- expect lean: unsupported
-- origin: INTERSECT ALL, once refused (#68), is its own IR node (docs/SOUNDNESS.md): set provers
--   refuse it, the cvc5 axis states it over bags
-- witness: t = {(1), (1)}; u = {(1), (1)}: A keeps both copies, B one
create table "t" ("a" INTEGER);
create table "u" ("a" INTEGER);
SELECT a FROM t INTERSECT ALL SELECT a FROM u;
SELECT a FROM t INTERSECT SELECT a FROM u;
