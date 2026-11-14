-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #236: a key pinned a joined row only when it was tied to the outer row, so a chain of joins on keys,
--   where the second key is tied to a column of the first joined row, was not read as at most one row
-- argument: shop.id and district.id are keys, so each sale meets at most one shop and that shop at most one district;
--   the join keeps each sale once or not at all, as EXISTS does
CREATE TABLE district (id int PRIMARY KEY, code text UNIQUE);
CREATE TABLE shop (id int PRIMARY KEY, district_id int NOT NULL, open boolean NOT NULL);
CREATE TABLE sale (id int PRIMARY KEY, shop_id int NOT NULL, amount int);
SELECT s.id, s.amount FROM sale s JOIN shop h ON h.id = s.shop_id JOIN district r ON r.id = h.district_id WHERE h.open AND r.code = $1;
SELECT s.id, s.amount FROM sale s WHERE EXISTS (SELECT 1 FROM shop h JOIN district r ON r.id = h.district_id WHERE h.id = s.shop_id AND h.open AND r.code = $1);
