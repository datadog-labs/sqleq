-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: PR #222 review: sqleq-check withheld from QED a plan whose text spelled a bag set
--   operation's node, and the literal 'exceptAll' is the same characters as the node's key, so
--   this pair, which has no bag set operation, was never handed to QED. Only object keys are matched
--   now (src/check/case.rs, bag_set_op)
create table "t" ("s" VARCHAR);
SELECT s FROM t WHERE s = 'exceptAll';
SELECT s FROM t WHERE 'exceptAll' = s;
