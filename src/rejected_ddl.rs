// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! What a raw-DDL statement the parser rejected may have done to the catalog, read off its first
//! words.
//!
//! Raw DDL is parsed one statement at a time, and a statement sqlparser cannot read is reported
//! rather than read (`pgddl::parse_reporting`). While the catalog read nothing but `CREATE TABLE`,
//! losing another statement cost nothing. It reads what follows a `CREATE TABLE` now, and losing one
//! of those can be unsound: pg_dump's `ALTER TABLE … ALTER COLUMN id ADD GENERATED … (SEQUENCE NAME
//! …)` leaves an identity column read as one with no default, and a rejected `DROP INDEX
//! CONCURRENTLY` a key the database no longer has.
//!
//! So a rejected statement is read as far as its head names what it touches, and only ever as a loss
//! of facts about that, which costs proofs and cannot make one. A head that names nothing readable
//! loses the facts of everything of its kind.

use sqlparser::ast::{Ident, ObjectName, ObjectNamePart};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::tokenizer::{Span, Token, Tokenizer};

/// One loss of facts a rejected statement may have caused, read by `catalog::build`.
#[derive(Clone, Debug, PartialEq)]
pub enum Rejection {
    /// `CREATE TABLE` or `CREATE VIEW` of this name: a relation whose columns are not known.
    Created(ObjectName),
    /// `ALTER TABLE` of this table in a way that may change its columns: they are not known.
    Altered(ObjectName),
    /// `ALTER TABLE … ALTER COLUMN c ADD GENERATED` or `SET DEFAULT`: the column's default may be a
    /// function of the row's position.
    Default(ObjectName, Ident),
    /// `CREATE TABLE … INHERITS (p)` or `ALTER TABLE … INHERIT p`: a scan of `p` reads another
    /// table's rows too.
    Inherited(ObjectName),
    /// `DROP TABLE`.
    Dropped(ObjectName),
    /// `CREATE TRIGGER … ON t`, or a rule on `t` for an event other than `SELECT`: DML on the table
    /// may store or touch other rows than it writes.
    Trigger(ObjectName),
    /// `DROP INDEX` or `ALTER INDEX`: the index's key may be gone.
    Index(ObjectName),
    /// `ALTER DOMAIN`: the domain's default may have changed.
    Domain(ObjectName),
    /// A statement of one of these kinds whose name is not readable: every table, index, domain or
    /// trigger.
    AnyTable,
    AnyIndex,
    AnyDomain,
    AnyTrigger,
}

/// The losses `sql`, a statement the parser rejected, may have caused.
pub fn read(sql: &str) -> Vec<Rejection> {
    let toks: Vec<Token> = match Tokenizer::new(&PostgreSqlDialect {}, sql).tokenize() {
        Ok(t) => t.into_iter().filter(|t| !matches!(t, Token::Whitespace(_))).collect(),
        // Not even its words are readable: its kind is all there is.
        Err(_) => return any_of_kind(sql),
    };
    let mut r = Reader { toks: &toks, i: 0 };
    let read = if r.words(&["ALTER", "TABLE"]) {
        r.alter_table()
    } else if r.words(&["DROP", "TABLE"]) {
        r.skip(&["IF", "EXISTS"]);
        r.names().map(|n| n.into_iter().map(Rejection::Dropped).collect())
    } else if r.words(&["DROP", "INDEX"]) {
        r.skip(&["CONCURRENTLY"]);
        r.skip(&["IF", "EXISTS"]);
        r.names().map(|n| n.into_iter().map(Rejection::Index).collect()).or(Some(vec![Rejection::AnyIndex]))
    } else if r.words(&["ALTER", "INDEX"]) {
        r.skip(&["IF", "EXISTS"]);
        Some(vec![r.name().map_or(Rejection::AnyIndex, Rejection::Index)])
    } else if r.words(&["ALTER", "DOMAIN"]) {
        Some(vec![r.name().map_or(Rejection::AnyDomain, Rejection::Domain)])
    } else if r.words(&["CREATE"]) {
        r.create()
    } else {
        Some(Vec::new())
    };
    read.unwrap_or_else(|| vec![Rejection::AnyTable])
}

/// The loss a statement whose words are not readable may have caused, by the kind its text starts
/// with.
fn any_of_kind(sql: &str) -> Vec<Rejection> {
    let head: Vec<String> = sql
        .split_whitespace()
        .map(str::to_ascii_uppercase)
        .filter(|w| !CREATE_MODIFIERS.contains(&w.as_str()))
        .take(2)
        .collect();
    match head.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["ALTER" | "DROP", "INDEX"] => vec![Rejection::AnyIndex],
        ["ALTER", "DOMAIN"] => vec![Rejection::AnyDomain],
        ["ALTER" | "DROP", "TABLE"] | ["CREATE", "TABLE" | "VIEW" | "RULE"] => vec![Rejection::AnyTable],
        ["CREATE", "TRIGGER"] => vec![Rejection::AnyTrigger],
        _ => Vec::new(),
    }
}

/// The words that may stand between `CREATE` and `TABLE`, `VIEW`, `TRIGGER` or `RULE`.
const CREATE_MODIFIERS: [&str; 11] = [
    "OR", "REPLACE", "GLOBAL", "LOCAL", "TEMP", "TEMPORARY", "UNLOGGED", "FOREIGN", "MATERIALIZED", "RECURSIVE",
    "CONSTRAINT",
];

struct Reader<'a> {
    toks: &'a [Token],
    i: usize,
}

/// Whether `t` is the unquoted keyword `k`.
fn is_word(t: Option<&Token>, k: &str) -> bool {
    matches!(t, Some(Token::Word(w)) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(k))
}

impl Reader<'_> {
    /// Consume `ks` if they come next, in order.
    fn words(&mut self, ks: &[&str]) -> bool {
        let all = ks.iter().enumerate().all(|(j, k)| is_word(self.toks.get(self.i + j), k));
        if all {
            self.i += ks.len();
        }
        all
    }

    fn skip(&mut self, ks: &[&str]) {
        self.words(ks);
    }

    fn at(&self, k: &str) -> bool {
        is_word(self.toks.get(self.i), k)
    }

    /// A possibly qualified name, its parts as written.
    fn name(&mut self) -> Option<ObjectName> {
        let mut parts = Vec::new();
        loop {
            let Some(Token::Word(w)) = self.toks.get(self.i) else { return None };
            parts.push(ObjectNamePart::Identifier(Ident {
                value: w.value.clone(),
                quote_style: w.quote_style,
                span: Span::empty(),
            }));
            self.i += 1;
            if self.toks.get(self.i) != Some(&Token::Period) {
                return Some(ObjectName(parts));
            }
            self.i += 1;
        }
    }

    /// Comma-separated names.
    fn names(&mut self) -> Option<Vec<ObjectName>> {
        let mut out = vec![self.name()?];
        while self.toks.get(self.i) == Some(&Token::Comma) {
            self.i += 1;
            out.push(self.name()?);
        }
        Some(out)
    }

    /// Whether a comma follows outside parentheses: an `ALTER TABLE` with more than one operation.
    fn more_than_one(&self) -> bool {
        let mut depth = 0i32;
        for t in &self.toks[self.i..] {
            match t {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                Token::Comma if depth == 0 => return true,
                _ => {}
            }
        }
        false
    }

    /// `ALTER TABLE`, after its first two words. `None` for a table name that is not readable.
    fn alter_table(&mut self) -> Option<Vec<Rejection>> {
        self.skip(&["IF", "EXISTS"]);
        self.skip(&["ONLY"]);
        let table = self.name()?;
        if self.toks.get(self.i) == Some(&Token::Mul) {
            self.i += 1;
        }
        let altered = || Some(vec![Rejection::Altered(table.clone())]);
        if self.more_than_one() {
            return altered();
        }
        if self.words(&["ALTER"]) {
            self.skip(&["COLUMN"]);
            let Some(ObjectNamePart::Identifier(col)) = self.name().and_then(|n| n.0.into_iter().next()) else {
                return altered();
            };
            if self.words(&["ADD", "GENERATED"]) || self.words(&["SET", "DEFAULT"]) {
                return Some(vec![Rejection::Default(table, col)]);
            }
            // What leaves the column's values, nullability and default as they were: storage and
            // statistics, and an identity column's sequence options. `DROP IDENTITY` and `DROP
            // EXPRESSION` leave a column with no default where it had a generated one, so reading it
            // as still generated only loses a fact. `SET NOT NULL` adds one, which is not read.
            const KEEP: [&str; 13] = [
                "STATISTICS", "STORAGE", "COMPRESSION", "GENERATED", "INCREMENT", "START", "MINVALUE",
                "MAXVALUE", "NO", "CYCLE", "CACHE", "RESTART", "NOT",
            ];
            let set_keeps = self.at("SET")
                && (KEEP.iter().any(|k| is_word(self.toks.get(self.i + 1), k))
                    || self.toks.get(self.i + 1) == Some(&Token::LParen));
            if set_keeps
                || self.at("RESET")
                || self.at("RESTART")
                || self.words(&["DROP", "IDENTITY"])
                || self.words(&["DROP", "EXPRESSION"])
            {
                return Some(Vec::new());
            }
            return altered();
        }
        if self.words(&["INHERIT"]) {
            return Some(vec![Rejection::Inherited(self.name()?)]);
        }
        // An added constraint only adds facts, which a rejection does not read.
        if self.at("ADD") {
            const CONSTRAINT: [&str; 6] = ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN", "EXCLUDE"];
            if CONSTRAINT.iter().any(|k| is_word(self.toks.get(self.i + 1), k)) {
                return Some(Vec::new());
            }
            return altered();
        }
        // What changes neither the table's columns nor what they hold: clustering, storage,
        // partitions (whose rows a partitioned table's own keys and NOT NULLs already cover),
        // ownership, triggers and policies.
        const KEEP: [&[&str]; 14] = [
            &["CLUSTER", "ON"],
            &["SET", "WITHOUT"],
            &["SET", "TABLESPACE"],
            &["SET", "ACCESS"],
            &["SET", "LOGGED"],
            &["SET", "UNLOGGED"],
            &["ATTACH", "PARTITION"],
            &["DETACH", "PARTITION"],
            &["OWNER", "TO"],
            &["REPLICA", "IDENTITY"],
            &["VALIDATE", "CONSTRAINT"],
            &["NO", "INHERIT"],
            &["ENABLE"],
            &["DISABLE"],
        ];
        let set_options = self.at("SET") && self.toks.get(self.i + 1) == Some(&Token::LParen);
        if set_options || self.at("RESET") || self.at("FORCE") || self.words(&["NO", "FORCE"]) {
            return Some(Vec::new());
        }
        if KEEP.iter().any(|ks| self.words(ks)) {
            return Some(Vec::new());
        }
        altered()
    }

    /// `CREATE TRIGGER name … ON table`, after `TRIGGER`: the table, past the timing and the events
    /// (`UPDATE OF a, b`), none of which says `ON` outside parentheses.
    fn trigger_table(&mut self) -> Option<ObjectName> {
        self.name()?;
        let toks = self.toks;
        let mut depth = 0i32;
        while self.i < toks.len() {
            match &toks[self.i] {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                _ if depth == 0 && self.at("ON") => {
                    self.i += 1;
                    return self.name();
                }
                _ => {}
            }
            self.i += 1;
        }
        None
    }

    /// `CREATE RULE name AS ON event TO table`, after `RULE`. A rule `ON SELECT` makes the table a
    /// view, whose columns are not known; any other rewrites the DML statements on it. `None` for a
    /// rule whose table is not readable.
    fn rule(&mut self) -> Option<Vec<Rejection>> {
        self.name()?;
        if !self.words(&["AS", "ON"]) {
            return None;
        }
        let select = self.at("SELECT");
        self.i += 1;
        if !self.words(&["TO"]) {
            return None;
        }
        let table = self.name()?;
        Some(vec![if select { Rejection::Altered(table) } else { Rejection::Trigger(table) }])
    }

    /// `CREATE`, after its first word: a table or view whose columns are not known, and the tables
    /// it inherits from. `None` for such a statement whose name is not readable.
    fn create(&mut self) -> Option<Vec<Rejection>> {
        while CREATE_MODIFIERS.iter().any(|k| self.at(k)) {
            self.i += 1;
        }
        if self.words(&["TRIGGER"]) {
            return Some(vec![self.trigger_table().map_or(Rejection::AnyTrigger, Rejection::Trigger)]);
        }
        if self.words(&["RULE"]) {
            return self.rule();
        }
        if !(self.words(&["TABLE"]) || self.words(&["VIEW"])) {
            return Some(Vec::new());
        }
        self.skip(&["IF", "NOT", "EXISTS"]);
        let mut out = vec![Rejection::Created(self.name()?)];
        let mut depth = 0i32;
        while self.i < self.toks.len() {
            match &self.toks[self.i] {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                _ if depth == 0 && self.at("INHERITS") => {
                    self.i += 1;
                    if self.toks.get(self.i) != Some(&Token::LParen) {
                        return None;
                    }
                    self.i += 1;
                    out.extend(self.names()?.into_iter().map(Rejection::Inherited));
                    if self.toks.get(self.i) != Some(&Token::RParen) {
                        return None;
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> ObjectName {
        ObjectName(s.split('.').map(|p| ObjectNamePart::Identifier(Ident::new(p))).collect())
    }

    #[test]
    fn pg_dumps_identity_column_is_a_default_of_its_column() {
        assert_eq!(
            read(
                "ALTER TABLE ONLY public.t ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (\n\
                   SEQUENCE NAME public.t_id_seq START WITH 1 INCREMENT BY 1 NO MINVALUE CACHE 1)"
            ),
            [Rejection::Default(name("public.t"), Ident::new("id"))]
        );
    }

    #[test]
    fn an_alter_table_that_may_change_columns_loses_the_table() {
        for sql in [
            "ALTER TABLE t SET SCHEMA s",
            "ALTER TABLE IF EXISTS ONLY t * ADD COLUMN c int GENERATED ALWAYS AS (a) VIRTUAL",
            "ALTER TABLE t ALTER COLUMN a TYPE int USING a::int, ADD COLUMN b int",
            "ALTER TABLE t ALTER COLUMN a SET STATISTICS 100, DROP COLUMN b",
            "ALTER TABLE t OF some_type_we_do_not_know AND MORE",
        ] {
            assert_eq!(read(sql), [Rejection::Altered(name("t"))], "{sql}");
        }
    }

    #[test]
    fn an_alter_table_that_keeps_columns_and_contents_is_nothing() {
        for sql in [
            "ALTER TABLE t CLUSTER ON t_pkey",
            "ALTER TABLE ONLY t ALTER COLUMN a SET STATISTICS 100",
            "ALTER TABLE t ALTER a SET STORAGE PLAIN",
            "ALTER TABLE t ALTER COLUMN id DROP IDENTITY IF EXISTS",
            "ALTER TABLE ONLY p ATTACH PARTITION c FOR VALUES FROM (1) TO (10)",
            "ALTER TABLE t ADD CONSTRAINT nn NOT NULL a",
            "ALTER TABLE t ADD CONSTRAINT k UNIQUE (a) WITH (fillfactor=70) USING INDEX TABLESPACE x",
            "ALTER TABLE t SET (fillfactor = 70)",
            "ALTER TABLE t NO INHERIT p",
        ] {
            assert_eq!(read(sql), [], "{sql}");
        }
    }

    #[test]
    fn inheritance_and_drops_are_read_by_name() {
        assert_eq!(read("ALTER TABLE c INHERIT s.p"), [Rejection::Inherited(name("s.p"))]);
        assert_eq!(
            read("CREATE TABLE c (LIKE o INCLUDING ALL, b int) INHERITS (p, s.q)"),
            [
                Rejection::Created(name("c")),
                Rejection::Inherited(name("p")),
                Rejection::Inherited(name("s.q"))
            ]
        );
        assert_eq!(
            read("DROP INDEX CONCURRENTLY IF EXISTS s.i, j"),
            [Rejection::Index(name("s.i")), Rejection::Index(name("j"))]
        );
        assert_eq!(read("ALTER INDEX public.i ATTACH PARTITION public.j"), [Rejection::Index(name("public.i"))]);
        assert_eq!(read("ALTER DOMAIN d SET DEFAULT nextval('s')"), [Rejection::Domain(name("d"))]);
        assert_eq!(read("CREATE FOREIGN TABLE f (a int) SERVER s"), [Rejection::Created(name("f"))]);
        assert_eq!(read("CREATE SEQUENCE s AS integer START WITH 1 INCREMENT BY 1"), []);
        assert_eq!(read("ALTER SEQUENCE s OWNED BY t.id"), []);
    }

    #[test]
    fn a_trigger_or_rule_names_its_table() {
        assert_eq!(
            read(
                "CREATE CONSTRAINT TRIGGER tr AFTER INSERT OR UPDATE OF a, b ON s.t FROM u \
                 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE PROCEDURE f()"
            ),
            [Rejection::Trigger(name("s.t"))]
        );
        assert_eq!(
            read("CREATE OR REPLACE RULE r AS ON INSERT TO t DO INSTEAD NOTHING"),
            [Rejection::Trigger(name("t"))]
        );
        assert_eq!(read("CREATE RULE r AS ON UPDATE TO t DO ALSO NOTIFY t"), [Rejection::Trigger(name("t"))]);
        // A rule `ON SELECT` makes the table a view.
        assert_eq!(
            read(r#"CREATE RULE "_RETURN" AS ON SELECT TO v DO INSTEAD SELECT 1 AS a"#),
            [Rejection::Altered(name("v"))]
        );
        assert_eq!(read("CREATE TRIGGER tr BEFORE INSERT ON"), [Rejection::AnyTrigger]);
        assert_eq!(read("CREATE TRIGGER tr BEFORE INSERT ON t WHEN (a = 'unterminated"), [Rejection::AnyTrigger]);
        assert_eq!(read("CREATE RULE r AS ON INSERT DO NOTHING"), [Rejection::AnyTable]);
    }

    #[test]
    fn a_name_that_is_not_readable_loses_its_whole_kind() {
        assert_eq!(read("ALTER TABLE 'oops' ADD COLUMN c int"), [Rejection::AnyTable]);
        assert_eq!(read("DROP INDEX CONCURRENTLY 42"), [Rejection::AnyIndex]);
        assert_eq!(read("ALTER TABLE t ADD CONSTRAINT c CHECK (a = 'unterminated"), [Rejection::AnyTable]);
        assert_eq!(read("CREATE TEMP TABLE 'oops' (a int"), [Rejection::AnyTable]);
        assert_eq!(read("CREATE FUNCTION f() RETURNS int AS $$ unterminated"), []);
        assert_eq!(read("GRANT SELECT ON t TO u"), []);
    }
}
