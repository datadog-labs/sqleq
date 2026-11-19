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
-- origin: a guard for FOREIGN KEY constraints: a NULL referencing value needs no parent row
-- witness: customers = {}, orders = {(1, NULL)}: A returns nothing, B returns 1
CREATE TABLE customers (id INTEGER PRIMARY KEY);
CREATE TABLE orders (id INTEGER PRIMARY KEY, cid INTEGER REFERENCES customers (id));
SELECT orders.id FROM orders JOIN customers ON orders.cid = customers.id;
SELECT id FROM orders;
