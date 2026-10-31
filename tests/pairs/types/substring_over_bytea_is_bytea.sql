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
-- origin: PR #209 review: SUBSTRING and TRIM were typed VARCHAR whatever their argument; over a bytea they
--   return a bytea, and are typed as their argument is (src/lower.rs, string_function_type)
-- witness: tb = {('\x61')}: A returns the one-byte bytea \x61, B returns the character \, the first
--   of the text '\x61'
create table tb (b bytea);
SELECT substring(b from 1 for 1) AS s FROM tb;
SELECT substring(b::text from 1 for 1) AS s FROM tb;
