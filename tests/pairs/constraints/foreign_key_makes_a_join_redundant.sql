-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: FOREIGN KEY constraints reach the plan, as references sqleq-cvc5 asserts
-- argument: every order's `cid` is non-null and names a customer, and `customers.id` is a key, so
--   the join keeps every order once
CREATE TABLE customers (id INTEGER PRIMARY KEY, name VARCHAR);
CREATE TABLE orders (id INTEGER PRIMARY KEY, cid INTEGER NOT NULL REFERENCES customers (id));
SELECT orders.id FROM orders JOIN customers ON orders.cid = customers.id;
SELECT id FROM orders;
