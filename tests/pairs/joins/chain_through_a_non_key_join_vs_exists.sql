-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #236: the negative side. The second join is on a column that is no key, so a shop meets several
--   districts and the join returns the sale once for each
-- witness: district = {(1, 'a'), (2, 'a')}, shop = {(1, 'a', true)}, sale = {(1, 1, 0)}: A returns the sale twice and
--   B once
CREATE TABLE district (id int PRIMARY KEY, region text);
CREATE TABLE shop (id int PRIMARY KEY, region text NOT NULL, open boolean NOT NULL);
CREATE TABLE sale (id int PRIMARY KEY, shop_id int NOT NULL, amount int);
SELECT s.id, s.amount FROM sale s JOIN shop h ON h.id = s.shop_id JOIN district r ON r.region = h.region WHERE h.open;
SELECT s.id, s.amount FROM sale s WHERE EXISTS (SELECT 1 FROM shop h JOIN district r ON r.region = h.region WHERE h.id = s.shop_id AND h.open);
