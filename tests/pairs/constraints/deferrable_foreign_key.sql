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
-- origin: a guard for FOREIGN KEY constraints: a DEFERRABLE one is checked at commit, so a query
--   inside the transaction can read an order with no customer
-- witness: customers = {}, orders = {(1, 1)} before the commit: A returns nothing, B returns 1
CREATE TABLE customers (id INTEGER PRIMARY KEY);
CREATE TABLE orders (id INTEGER PRIMARY KEY,
  cid INTEGER NOT NULL REFERENCES customers (id) DEFERRABLE INITIALLY DEFERRED);
SELECT orders.id FROM orders JOIN customers ON orders.cid = customers.id;
SELECT id FROM orders;
