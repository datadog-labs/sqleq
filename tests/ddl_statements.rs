// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! What follows a `CREATE TABLE` in the DDL is read, in order, unique indexes included (issue #92).
//!
//! The catalog used to read `CREATE TABLE` and nothing after it, so an `ALTER TABLE` that dropped a
//! key, a `NOT NULL` or a column, or gave a column a sequence default, was not seen, and every axis
//! proved pairs over the table as it had been created. Each test reads one DDL as a pair file and as
//! raw DDL and expects one answer, except the last few: only raw DDL is parsed a statement at a
//! time, so only it has statements the parser rejects. They do not run a prover.

use serde_json::{json, Value};
use sqleq_frontend::{lower_with, lower_with_ddl, CatalogSource, FrontendError, Result};

/// The pair `q0`/`q1` over `ddl`, lowered from a pair file and from raw DDL.
fn both(ddl: &str, q0: &str, q1: &str) -> [Result<Value>; 2] {
    [
        lower_with(&format!("{ddl}\n{q0};\n{q1};"), CatalogSource::Declared),
        lower_with_ddl(&format!("{q0};\n{q1};"), ddl, CatalogSource::Declared),
    ]
}

fn lowered(ddl: &str, q0: &str, q1: &str) -> [Value; 2] {
    both(ddl, q0, q1).map(|r| r.unwrap_or_else(|e| panic!("expected Ok, got {e}\n{ddl}\n{q0}\n{q1}")))
}

fn expect_refusal(r: Result<Value>, needle: &str, ddl: &str) {
    match r {
        Err(FrontendError::Unsupported(m) | FrontendError::Schema(m)) => {
            assert!(m.contains(needle), "refused for {m:?}, expected {needle:?}\n{ddl}")
        }
        Err(e) => panic!("expected a refusal mentioning {needle:?}, got {e}\n{ddl}"),
        Ok(_) => panic!("expected a refusal mentioning {needle:?}, but it lowered\n{ddl}"),
    }
}

fn refused(ddl: &str, q0: &str, q1: &str, needle: &str) {
    for r in both(ddl, q0, q1) {
        expect_refusal(r, needle, ddl);
    }
}

/// The schema of table `name` in a lowered pair.
fn schema<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["schemas"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("no {name}: {v}"))
}

/// The keys and the nullability `ddl` gives table `t`, read both ways off a pair over `t(id, a)`.
fn keys_and_nullable(ddl: &str) -> (Value, Value) {
    let [a, b] = lowered(ddl, "SELECT id FROM t", "SELECT id FROM t WHERE a > 1");
    let read = |v: &Value| (schema(v, "t")["key"].clone(), schema(v, "t")["nullable"].clone());
    assert_eq!(read(&a), read(&b), "the two inputs disagree\n{ddl}");
    read(&a)
}

const INSERT: [&str; 2] = ["INSERT INTO t (a) VALUES (1), (2)", "INSERT INTO t (a) VALUES (2), (1)"];
const VOLATILE: &str = "whose default is not a function of the row";
const UNREAD: &str = "declared, but its columns are not read";

#[test]
fn alter_table_adds_a_key_and_a_not_null() {
    let ddl = "CREATE TABLE t (id integer, a integer); ALTER TABLE t ADD PRIMARY KEY (id);";
    assert_eq!(keys_and_nullable(ddl), (json!([[0]]), json!([false, true])));
    let ddl = "CREATE TABLE t (id integer NOT NULL, a integer); \
               ALTER TABLE ONLY public.t ADD CONSTRAINT t_id UNIQUE (id); \
               ALTER TABLE t ALTER COLUMN a SET NOT NULL;";
    assert_eq!(keys_and_nullable(ddl), (json!([[0]]), json!([false, false])));
}

#[test]
fn a_later_statement_undoes_an_earlier_one() {
    let ddl = "CREATE TABLE t (id integer, a integer NOT NULL, CONSTRAINT t_pk PRIMARY KEY (id)); \
               ALTER TABLE t DROP CONSTRAINT t_pk; ALTER TABLE t ALTER COLUMN a DROP NOT NULL;";
    assert_eq!(keys_and_nullable(ddl), (json!([]), json!([true, true])), "the key's NOT NULL goes with it");
    let ddl = "CREATE TABLE t (id integer NOT NULL CONSTRAINT t_pk PRIMARY KEY, a integer); \
               ALTER TABLE t DROP CONSTRAINT t_pk;";
    assert_eq!(keys_and_nullable(ddl), (json!([]), json!([false, true])), "a NOT NULL of its own stays");
    let ddl = "CREATE TABLE t (id integer NOT NULL, a integer); \
               ALTER TABLE t ADD CONSTRAINT k UNIQUE (id); ALTER TABLE t RENAME CONSTRAINT k TO j; \
               ALTER TABLE t DROP CONSTRAINT j;";
    assert_eq!(keys_and_nullable(ddl), (json!([]), json!([false, true])));
}

#[test]
fn a_constraint_name_the_reader_does_not_know_takes_every_key_and_not_null() {
    let ddl = "CREATE TABLE t (id integer PRIMARY KEY, a integer NOT NULL UNIQUE, CONSTRAINT c CHECK (a > 0))";
    assert_eq!(keys_and_nullable(&format!("{ddl}; ALTER TABLE t DROP CONSTRAINT c;")).0, json!([[0], [1]]));
    // `t_pkey` is the name Postgres gave the unnamed key; from Postgres 18 a NOT NULL has one too.
    assert_eq!(
        keys_and_nullable(&format!("{ddl}; ALTER TABLE t DROP CONSTRAINT t_pkey;")),
        (json!([]), json!([true, true]))
    );
}

#[test]
fn a_default_set_after_create_table_reaches_the_insert_reduction() {
    let table = "CREATE SEQUENCE s; CREATE TABLE t (id integer NOT NULL, a integer)";
    let [q0, q1] = INSERT;
    refused(&format!("{table}; ALTER TABLE t ALTER COLUMN id SET DEFAULT nextval('s');"), q0, q1, VOLATILE);
    refused(&format!("{table}; ALTER TABLE t ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY;"), q0, q1, VOLATILE);
    lowered(
        &format!(
            "{table}; ALTER TABLE t ALTER COLUMN id SET DEFAULT nextval('s'); \
             ALTER TABLE t ALTER COLUMN id DROP DEFAULT;"
        ),
        q0,
        q1,
    );
    lowered("CREATE TABLE t (id serial, a integer); ALTER TABLE t ALTER COLUMN id SET DEFAULT 0;", q0, q1);
    refused(r#"CREATE TABLE t ("id" "serial", a integer);"#, q0, q1, VOLATILE);
}

#[test]
fn a_domain_default_is_a_default_of_its_columns() {
    let domain = "CREATE SEQUENCE s; CREATE DOMAIN d AS integer DEFAULT nextval('s'); CREATE DOMAIN e AS d";
    let [q0, q1] = INSERT;
    refused(&format!("{domain}; CREATE TABLE t (id d, a integer);"), q0, q1, VOLATILE);
    refused(&format!("{domain}; CREATE TABLE t (id e, a integer);"), q0, q1, VOLATILE);
    // A column's own default is the one it takes, and dropping it gives it the domain's again.
    lowered(&format!("{domain}; CREATE TABLE t (id d DEFAULT 0, a integer);"), q0, q1);
    refused(
        &format!("{domain}; CREATE TABLE t (id d DEFAULT 0, a integer); ALTER TABLE t ALTER COLUMN id DROP DEFAULT;"),
        q0,
        q1,
        VOLATILE,
    );
    lowered("CREATE DOMAIN d AS integer DEFAULT 0; CREATE TABLE t (id d, a integer);", q0, q1);
}

#[test]
fn an_operation_that_changes_the_columns_leaves_the_table_unread() {
    let table = "CREATE TABLE t (id integer PRIMARY KEY, a integer)";
    for op in [
        "ADD COLUMN b integer",
        "DROP COLUMN a",
        "RENAME COLUMN a TO b",
        "ALTER COLUMN a TYPE bigint",
        "ALTER COLUMN nosuch DROP NOT NULL",
        "ADD COLUMN b integer, OWNER TO u",
    ] {
        refused(&format!("{table}; ALTER TABLE t {op};"), "SELECT id FROM t", "SELECT id FROM t WHERE a > 1", UNREAD);
    }
    // A rename keys the table on its new name, in its schema.
    let ddl = "CREATE TABLE s.t (id integer PRIMARY KEY, a integer); ALTER TABLE s.t RENAME TO u;";
    for v in lowered(ddl, "SELECT id FROM s.u", "SELECT id FROM s.u WHERE a > 1") {
        assert_eq!(schema(&v, "s.u")["key"], json!([[0]]));
    }
    refused(ddl, "SELECT id FROM s.t", "SELECT id FROM s.t WHERE a > 1", "unknown table");
}

#[test]
fn an_operation_that_changes_nothing_read_changes_nothing() {
    let ddl = "CREATE TABLE t (id integer PRIMARY KEY, a integer); \
               ALTER TABLE t OWNER TO u; ALTER TABLE t ENABLE TRIGGER x; ALTER TABLE t REPLICA IDENTITY FULL;";
    assert_eq!(keys_and_nullable(ddl), (json!([[0]]), json!([false, true])));
}

#[test]
fn an_alter_table_whose_table_does_not_resolve_leaves_each_of_its_name_unread() {
    let ddl = "CREATE TABLE a.t (id integer PRIMARY KEY, a integer); CREATE TABLE b.t (id integer PRIMARY KEY, a integer)";
    let q = ["SELECT id FROM a.t", "SELECT id FROM a.t WHERE a > 1"];
    refused(&format!("{ddl}; ALTER TABLE t DROP CONSTRAINT t_pkey;"), q[0], q[1], UNREAD);
    // Unless it only adds facts, which it does to no table.
    for v in lowered(&format!("{ddl}; ALTER TABLE t ALTER COLUMN a SET NOT NULL;"), q[0], q[1]) {
        assert_eq!(schema(&v, "a.t")["nullable"], json!([false, true]));
    }
}

#[test]
fn drop_table_and_drop_view_drop_the_relation() {
    let ddl = "CREATE TABLE t (id integer PRIMARY KEY, a integer); DROP TABLE t; CREATE TABLE t (id integer, a integer);";
    assert_eq!(keys_and_nullable(ddl), (json!([]), json!([true, true])));
    let ddl = "CREATE TABLE s.t (id integer PRIMARY KEY, a integer); CREATE VIEW t AS SELECT 1 AS id, 2 AS a; DROP VIEW t;";
    for v in lowered(ddl, "SELECT id FROM t", "SELECT id FROM t WHERE a > 1") {
        assert_eq!(schema(&v, "s.t")["key"], json!([[0]]));
    }
}

#[test]
fn a_view_or_a_table_without_its_columns_takes_its_name_from_another_schemas() {
    let q = ["SELECT DISTINCT id FROM t", "SELECT id FROM t"];
    for unread in [
        "CREATE VIEW t AS SELECT 1 AS id",
        "CREATE TABLE o (id integer); CREATE TABLE t (LIKE o)",
        "CREATE TABLE o (id integer); CREATE TABLE t (x integer, LIKE o)",
        "CREATE TABLE o (id integer); CREATE TABLE t AS SELECT id FROM o",
    ] {
        let ddl = format!("CREATE TABLE s.t (id integer PRIMARY KEY); {unread};");
        refused(&ddl, q[0], q[1], UNREAD);
        // A qualified reference still reads the qualified table.
        lowered(&ddl, "SELECT DISTINCT id FROM s.t", "SELECT id FROM s.t");
    }
}

#[test]
fn a_table_another_inherits_from_keeps_no_key_and_the_inheriting_one_is_unread() {
    let ddl = "CREATE TABLE p (id integer PRIMARY KEY, a integer NOT NULL); CREATE TABLE c (x integer) INHERITS (p);";
    for v in lowered(ddl, "SELECT id FROM p", "SELECT id FROM p WHERE a > 1") {
        assert_eq!(schema(&v, "p")["key"], json!([]));
        assert_eq!(schema(&v, "p")["nullable"], json!([true, true]));
    }
    refused(ddl, "SELECT x FROM c", "SELECT x FROM c WHERE x > 1", UNREAD);
}

#[test]
fn a_key_added_to_a_partitioned_table_alone_is_not_read() {
    let table = "CREATE TABLE t (id integer NOT NULL, a integer) PARTITION BY RANGE (id)";
    assert_eq!(keys_and_nullable(&format!("{table}; ALTER TABLE ONLY t ADD PRIMARY KEY (id);")).0, json!([]));
    assert_eq!(keys_and_nullable(&format!("{table}; ALTER TABLE t ADD PRIMARY KEY (id);")).0, json!([[0]]));
}

#[test]
fn a_key_is_every_part_or_nothing() {
    // Postgres refuses a constraint over a column the table does not have; the reader, finding one,
    // reads no key and no NOT NULL rather than a key over the columns it found.
    let ddl = "CREATE TABLE t (id integer, a integer, PRIMARY KEY (id, nosuch));";
    assert_eq!(keys_and_nullable(ddl), (json!([]), json!([true, true])));
    let ddl = "CREATE TABLE t (id integer NOT NULL, a integer NOT NULL); ALTER TABLE t ADD UNIQUE (id, nosuch);";
    assert_eq!(keys_and_nullable(ddl).0, json!([]));
}

#[test]
fn a_unique_index_over_plain_columns_is_a_key() {
    let table = "CREATE TABLE t (id integer NOT NULL, a integer NOT NULL)";
    for (index, key) in [
        ("CREATE UNIQUE INDEX i ON t (id)", json!([[0]])),
        ("CREATE UNIQUE INDEX i ON t USING btree (id DESC NULLS LAST, a)", json!([[0, 1]])),
        ("CREATE UNIQUE INDEX i ON t (id) INCLUDE (a)", json!([[0]])),
        // An index is found as a query's reference finds its table.
        ("CREATE UNIQUE INDEX i ON public.t (id)", json!([[0]])),
        ("CREATE UNIQUE INDEX i ON shop.t (id)", json!([[0]])),
        ("CREATE UNIQUE INDEX i ON t (id) NULLS NOT DISTINCT", json!([[0]])),
        ("CREATE UNIQUE INDEX CONCURRENTLY i ON t (id)", json!([[0]])),
        // Partial, under an operator class or a collation, over an expression, or maybe not created.
        ("CREATE UNIQUE INDEX i ON t (id) WHERE a > 0", json!([])),
        ("CREATE UNIQUE INDEX i ON t (id int4_ops)", json!([])),
        ("CREATE UNIQUE INDEX i ON t ((id + 0))", json!([])),
        ("CREATE UNIQUE INDEX i ON t (id, (a + 0))", json!([])),
        ("CREATE UNIQUE INDEX IF NOT EXISTS i ON t (id)", json!([])),
        ("CREATE INDEX i ON t (id)", json!([])),
    ] {
        assert_eq!(keys_and_nullable(&format!("{table}; {index};")).0, key, "{index}");
    }
    let ddl = "CREATE TABLE t (id text NOT NULL, a integer); CREATE UNIQUE INDEX i ON t (id COLLATE \"C\");";
    assert_eq!(keys_and_nullable(ddl).0, json!([]));
}

#[test]
fn a_dropped_or_renamed_unique_index_is_followed() {
    let table = "CREATE TABLE t (id integer NOT NULL, a integer NOT NULL)";
    for (rest, key) in [
        ("CREATE UNIQUE INDEX i ON t (id); DROP INDEX i", json!([])),
        ("CREATE UNIQUE INDEX i ON t (id); DROP INDEX IF EXISTS public.i CASCADE", json!([])),
        ("CREATE UNIQUE INDEX i ON t (id); ALTER INDEX i RENAME TO j; DROP INDEX j", json!([])),
        ("CREATE UNIQUE INDEX i ON t (id); ALTER INDEX i RENAME TO j; DROP INDEX i", json!([[0]])),
        // An unnamed index's name is Postgres's to generate, so any DROP INDEX may be of it.
        ("CREATE UNIQUE INDEX ON t (id); DROP INDEX some_other_index", json!([])),
        ("CREATE UNIQUE INDEX i ON t (id); DROP INDEX some_other_index", json!([[0]])),
    ] {
        assert_eq!(keys_and_nullable(&format!("{table}; {rest};")).0, key, "{rest}");
    }
}

#[test]
fn a_constraint_using_an_index_takes_its_key() {
    let ddl = "CREATE TABLE t (id integer, a integer); CREATE UNIQUE INDEX i ON t (id); \
               ALTER TABLE t ADD CONSTRAINT t_pk PRIMARY KEY USING INDEX i;";
    assert_eq!(keys_and_nullable(ddl), (json!([[0]]), json!([false, true])), "a primary key is NOT NULL");
    assert_eq!(keys_and_nullable(&format!("{ddl} ALTER TABLE t DROP CONSTRAINT t_pk;")).0, json!([]));
    let ddl = "CREATE TABLE t (id integer NOT NULL, a integer); CREATE UNIQUE INDEX i ON t (id); \
               ALTER TABLE t ADD CONSTRAINT u UNIQUE USING INDEX i DEFERRABLE;";
    assert_eq!(keys_and_nullable(ddl).0, json!([]), "a deferrable constraint is no key");
}

#[test]
fn serial_and_identity_columns_are_not_null() {
    let nullable = |ddl: &str| {
        let [a, b] = lowered(ddl, "SELECT b FROM t", "SELECT b FROM t WHERE b > 1");
        assert_eq!(schema(&a, "t")["nullable"], schema(&b, "t")["nullable"], "{ddl}");
        schema(&a, "t")["nullable"].clone()
    };
    for col in [
        "id serial",
        "id BIGSERIAL",
        "id smallserial",
        "id serial8",
        r#"id "serial""#,
        "id integer GENERATED ALWAYS AS IDENTITY",
        "id bigint GENERATED BY DEFAULT AS IDENTITY (START WITH 10)",
    ] {
        assert_eq!(nullable(&format!("CREATE TABLE t ({col}, b integer);")), json!([false, true]), "{col}");
    }
    for col in [
        "id integer DEFAULT nextval('s')",
        "id integer GENERATED ALWAYS AS (b + 1) STORED",
        // Quoted, the upper-case name is not the keyword: some other type.
        r#"id "SERIAL""#,
    ] {
        assert_eq!(nullable(&format!("CREATE TABLE t ({col}, b integer);")), json!([true, true]), "{col}");
    }
}

// Raw DDL alone: a statement its parser rejects.

fn raw(ddl: &str, q0: &str, q1: &str) -> Result<Value> {
    lower_with_ddl(&format!("{q0};\n{q1};"), ddl, CatalogSource::Declared)
}

#[test]
fn pg_dumps_identity_column_reaches_the_insert_reduction() {
    // sqlparser rejects the sequence options; the column is an identity column all the same.
    let ddl = "CREATE TABLE public.t (id integer NOT NULL, a integer); \
               ALTER TABLE ONLY public.t ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (\n\
                 SEQUENCE NAME public.t_id_seq START WITH 1 INCREMENT BY 1 NO MINVALUE NO MAXVALUE CACHE 1);";
    let [q0, q1] = INSERT;
    expect_refusal(raw(ddl, q0, q1), VOLATILE, ddl);
}

#[test]
fn a_rejected_statement_is_read_as_a_loss_of_what_it_names() {
    let table = "CREATE TABLE t (id integer PRIMARY KEY, a integer)";
    let q = ["SELECT id FROM t", "SELECT id FROM t WHERE a > 1"];
    // One that may change the columns leaves the table unread.
    let ddl = format!("{table}; ALTER TABLE t SET SCHEMA s;");
    expect_refusal(raw(&ddl, q[0], q[1]), UNREAD, &ddl);
    // One that changes nothing read changes nothing.
    let ddl = format!("{table}; ALTER TABLE t CLUSTER ON t_pkey;");
    assert_eq!(schema(&raw(&ddl, q[0], q[1]).unwrap(), "t")["key"], json!([[0]]));
    // A rejected `INHERIT` makes the parent keep no key.
    let ddl = format!("{table}; CREATE TABLE c (id integer, a integer); ALTER TABLE c INHERIT t;");
    assert_eq!(schema(&raw(&ddl, q[0], q[1]).unwrap(), "t")["key"], json!([]));
    // A rejected `CREATE TABLE` takes its name from another schema's table.
    let ddl = "CREATE TABLE s.t (id integer PRIMARY KEY, a integer); CREATE TABLE o (id integer); \
               CREATE TABLE t (LIKE o INCLUDING ALL, a integer);";
    expect_refusal(raw(ddl, q[0], q[1]), UNREAD, ddl);
    // A rejected `ALTER DOMAIN` may have given the domain a sequence default.
    let ddl = "CREATE DOMAIN d AS integer; CREATE TABLE t (id d, a integer); ALTER DOMAIN d SET DEFAULT nextval('s');";
    let [q0, q1] = INSERT;
    expect_refusal(raw(ddl, q0, q1), VOLATILE, ddl);
}

#[test]
fn a_rejected_index_statement_only_loses_keys() {
    let table = "CREATE TABLE t (id integer NOT NULL, a integer); CREATE UNIQUE INDEX i ON t (id)";
    let q = ["SELECT id FROM t", "SELECT id FROM t WHERE a > 1"];
    let key = |ddl: &str| schema(&raw(ddl, q[0], q[1]).unwrap(), "t")["key"].clone();
    assert_eq!(key(&format!("{table};")), json!([[0]]));
    assert_eq!(key(&format!("{table}; DROP INDEX CONCURRENTLY i;")), json!([]));
    assert_eq!(key(&format!("{table}; ALTER INDEX i SET (fillfactor = 70) AND MORE;")), json!([]));
    // An index on a partitioned table alone (`ON ONLY`) is rejected, and gives no key.
    assert_eq!(key("CREATE TABLE t (id integer NOT NULL, a integer); CREATE UNIQUE INDEX i ON ONLY t (id);"), json!([]));
}
