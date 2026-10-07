-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-schema
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- catalog: inferred
-- origin: issue #112: with "Orders" and orders one synthesized table, the two sides were the
--   two columns of a self-join, which both provers proved equal as bags
-- witness: "Orders" = {(1)}, orders = {(2)}: A returns 1, B returns 2
SELECT "Orders"."id" FROM "Orders", "orders";
SELECT "orders"."id" FROM "Orders", "orders";
