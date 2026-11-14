-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #233: oid was classed as an integer, so a cast to it was a CAST between equal IR types in the declared
--   catalog, which a prover reads as the identity, and was dropped as a no-op in the inferring ones. oid is unsigned:
--   a negative integer cast to it wraps
-- witness: t = {(1, -1)}: CAST(-1 AS oid) is 4294967295, so A returns 1 and B returns no row
CREATE TABLE t (id int PRIMARY KEY, a int);
SELECT id FROM t WHERE CAST(a AS oid) > 0;
SELECT id FROM t WHERE a > 0;
