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
-- origin: PR #209 review, split: TRIM was refused outright, and once lowered was typed VARCHAR whatever
--   its argument; over a bytea it returns a bytea, and is typed as its argument is (src/lower.rs,
--   string_function_type)
-- witness: tb = {('\x6161')}: A trims the byte 0x61 from both ends and returns the empty bytea \x; B
--   trims the character a from the text '\x6161', which has none, and returns that text
create table tb (b bytea);
SELECT trim('\x61'::bytea FROM b) AS s FROM tb;
SELECT trim('a' FROM b::text) AS s FROM tb;
