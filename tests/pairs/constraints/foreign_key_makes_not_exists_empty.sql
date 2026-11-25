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
-- origin: a FOREIGN KEY added by ALTER TABLE, read under NOT EXISTS
-- argument: every order's non-null `cid` is a customer's `id`, so no order passes the NOT EXISTS
CREATE TABLE customers (id INTEGER PRIMARY KEY);
CREATE TABLE orders (id INTEGER PRIMARY KEY, cid INTEGER NOT NULL);
ALTER TABLE orders ADD FOREIGN KEY (cid) REFERENCES customers (id);
SELECT id FROM orders o WHERE NOT EXISTS (SELECT 1 FROM customers c WHERE c.id = o.cid);
SELECT id FROM orders WHERE false;
