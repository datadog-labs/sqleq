// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The Postgres engine (`--engine postgres`): run a pair on a private PostgreSQL cluster, where what
//! Postgres computes is the answer and nothing has to be emulated.
//!
//! The instance generator, the parameter analysis and the cut and nondeterminism rules are the ones
//! [`crate::test_pair`] uses. What differs is where and how the statements run:
//!
//! * [`Server`] serves a copy of a cached template cluster from a fresh temp directory, on a unix
//!   socket there only, with one database per worker, and is gone when dropped or when the process
//!   dies.
//! * The DDL runs once per pair, inside a transaction rolled back at the end, so every constraint it
//!   declares is Postgres's to enforce and a generated row Postgres rejects is simply not there.
//!   Captured DDL is repaired only in ways that add no constraint: a schema it names is created, a
//!   table the rest of the DDL names by one schema is created there, a table the queries name by one
//!   schema is moved there, a type nothing declares is a `text` domain, and a column default that
//!   calls a function nothing declares is dropped ([`Timing::caveat`] reports the last two).
//! * Each trial loads its rows under a savepoint and runs each side under a nested one, so nothing is
//!   re-created per side, and a sequence is reset before each side that can write.
//! * Each placeholder is written in as a cast to the type Postgres infers for it, one type per `$N`
//!   across the pair.
//! * Results are read as Postgres's own text. Two bags of one size that differ only in spelling are
//!   compared again under Postgres `=`, so `2.0` and `2.00` are the same value.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::LazyLock;
use std::time::Instant;

use postgres::{Client, NoTls, SimpleQueryMessage};
use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};
use regex::Regex;

use crate::duck::{table_forms, RowData};
use crate::gen::{
    array_element_type, cast_target, clocks, randval, randval_cast, randval_col, randval_need,
    CastTarget, Val,
};
use crate::limits::{self, Count};
use crate::pair::{is_query, small_size, Config, Verdict, SMALL_STREAM};
use crate::patterns as pat;
use crate::schema::{parse_schema, VType};
use crate::typing;

/// Contrib extensions captured schemas commonly use, installed into the template every worker
/// database is cloned from.
const EXTENSIONS: &[&str] = &[
    "citext",
    "pg_trgm",
    "uuid-ossp",
    "hstore",
    "pgcrypto",
    "ltree",
    "unaccent",
    "btree_gist",
    "btree_gin",
];

/// The Postgres major version verdicts are defined against.
pub const MAJOR: u32 = 17;

/// Bumped whenever what the cached template holds changes, so an old template is never reused.
const TEMPLATE_FORMAT: u32 = 3;

/// Where the Postgres binaries are: `$SQLEQ_PG_BIN` when it is set, else the PostgreSQL the build
/// fetched (`build.rs`) if it runs here, else the `postgres` on `PATH`.
///
/// The fetched one sits under `target/<profile>/postgresql/`, which this executable finds from its
/// own directory: `target/<profile>/` for a binary, `target/<profile>/deps/` for a test harness, or
/// wherever a binary was copied together with that `postgresql/` directory. A Linux build of it uses
/// the system's own OpenSSL, libxml2, Kerberos, zstd and lz4, so on a machine without one of them
/// it cannot start, and `PATH` is tried instead.
pub fn bin_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("SQLEQ_PG_BIN") {
        return Ok(PathBuf::from(dir));
    }
    let mut fetched_fails = None;
    if let Some(dir) = fetched() {
        match version(&dir) {
            Ok(_) => return Ok(dir),
            Err(e) => fetched_fails = Some(e),
        }
    }
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .find(|d| d.join("postgres").is_file() && d.join("initdb").is_file())
        })
        .ok_or_else(|| match fetched_fails {
            Some(e) => format!(
                "the PostgreSQL the build fetched cannot run here ({e}), and none is on PATH: \
                 install the libraries it needs, or set SQLEQ_PG_BIN to a PostgreSQL {MAJOR}"
            ),
            None => format!(
                "no PostgreSQL {MAJOR} found: set SQLEQ_PG_BIN to the directory holding its \
                 `postgres` and `initdb`"
            ),
        })
}

/// The `bin` directory of the PostgreSQL the build fetched, if it can be found from here.
fn fetched() -> Option<PathBuf> {
    let rel = option_env!("SQLEQ_PG_FETCHED")?;
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let found = [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .map(|d| d.join(rel))
        .find(|b| b.join("postgres").is_file());
    found
}

/// The server's version (`17.11`), refusing any major but [`MAJOR`]: a verdict is a claim about one
/// version's semantics.
pub fn version(bin: &Path) -> Result<String, String> {
    let out = Command::new(bin.join("postgres"))
        .arg("--version")
        .output()
        .map_err(|e| format!("cannot run {}: {e}", bin.join("postgres").display()))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{} does not start: {}",
            bin.join("postgres").display(),
            why.lines().next().unwrap_or("").trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let ver = text
        .split_whitespace()
        .last()
        .unwrap_or("")
        .to_string();
    let major: Option<u32> = ver.split('.').next().and_then(|m| m.parse().ok());
    if major != Some(MAJOR) {
        return Err(format!(
            "sqleq-fuzz needs PostgreSQL {MAJOR}, and {} is `{}`",
            bin.join("postgres").display(),
            text.trim()
        ));
    }
    Ok(ver)
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{:?} failed: {}",
            cmd.get_program(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// A fresh directory under the system temp dir, named for this process.
fn fresh_dir(prefix: &str) -> Result<PathBuf, String> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "{prefix}{}-{}-{nanos:x}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The prefix of every directory a run serves a cluster from.
const RUN_PREFIX: &str = "sqleq-pg-";

/// Remove the run directories of processes that died without removing theirs (a timeout's kill):
/// those whose socket nothing answers on, left more than ten minutes ago.
fn sweep_stale_runs() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(RUN_PREFIX) {
            continue;
        }
        let path = entry.path();
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(600));
        let live = std::os::unix::net::UnixStream::connect(path.join(".s.PGSQL.5432")).is_ok();
        if old && !live {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// A postmaster serving `data` on a unix socket in `sock` only, run under a shell that stops it the
/// moment its stdin closes -- when the [`Postmaster`] is dropped, and equally when this process is
/// killed, which `pg_ctl`'s detached server would outlive.
struct Postmaster {
    supervisor: std::process::Child,
}

impl Postmaster {
    fn start(bin: &Path, data: &Path, sock: &Path, max_connections: usize) -> Result<Postmaster, String> {
        let log = std::fs::File::create(sock.join("server.log")).map_err(|e| e.to_string())?;
        let supervisor = Command::new("sh")
            .arg("-c")
            .arg(r#""$@" & pid=$!; read -r _; kill -INT "$pid" 2>/dev/null; wait "$pid""#)
            .arg("sh")
            .arg(bin.join("postgres"))
            .arg("-D")
            .arg(data)
            .args(["-c", "listen_addresses=", "-c"])
            .arg(format!("unix_socket_directories={}", sock.display()))
            .args([
                "-c",
                "port=5432",
                "-c",
                "fsync=off",
                "-c",
                "synchronous_commit=off",
                "-c",
                "full_page_writes=off",
                "-c",
                "jit=off",
                "-c",
                "dynamic_shared_memory_type=mmap",
                "-c",
                "shared_buffers=32MB",
                "-c",
                "TimeZone=UTC",
                "-c",
                "lc_messages=C",
                // Errors are the common case here -- a row a constraint refuses, a side Postgres
                // rejects -- so none is logged: a corpus run would otherwise write every failing
                // statement's text to the log.
                "-c",
                "log_min_messages=fatal",
                "-c",
                "log_min_error_statement=panic",
                // A pair's DDL creates every table, index and type in one transaction, which can
                // take more locks than the default lock table holds.
                "-c",
                "max_locks_per_transaction=1024",
                "-c",
            ])
            .arg(format!("max_connections={max_connections}"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|e| format!("cannot start postgres: {e}"))?;
        let pm = Postmaster { supervisor };
        // Ready once a connection is accepted.
        let started = Instant::now();
        loop {
            if Client::connect(&conninfo(sock, "postgres"), NoTls).is_ok() {
                return Ok(pm);
            }
            if started.elapsed() > std::time::Duration::from_secs(30) {
                let log = std::fs::read_to_string(sock.join("server.log")).unwrap_or_default();
                return Err(format!("postgres did not start: {}", log.trim()));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

impl Drop for Postmaster {
    fn drop(&mut self) {
        drop(self.supervisor.stdin.take());
        let _ = self.supervisor.wait();
    }
}

fn conninfo(sock: &Path, db: &str) -> String {
    format!("host={} port=5432 user=postgres dbname={db}", sock.display())
}

/// The `uuid-ossp` functions core PostgreSQL computes exactly, defined under their own names where the
/// extension cannot load: the one a build carries may need a library the system lacks (the fetched
/// Linux build links OSSP's libuuid), and a schema that calls them must run on every build alike.
/// `gen_random_uuid()` draws a version-4 uuid as `uuid_generate_v4()` does; the namespace constants
/// are RFC 4122's.
const UUID_OSSP_CORE: &str = "\
    CREATE FUNCTION uuid_generate_v4() RETURNS uuid LANGUAGE sql VOLATILE PARALLEL SAFE \
        AS 'SELECT gen_random_uuid()'; \
    CREATE FUNCTION uuid_nil() RETURNS uuid LANGUAGE sql IMMUTABLE PARALLEL SAFE \
        AS $$SELECT '00000000-0000-0000-0000-000000000000'::uuid$$; \
    CREATE FUNCTION uuid_ns_dns() RETURNS uuid LANGUAGE sql IMMUTABLE PARALLEL SAFE \
        AS $$SELECT '6ba7b810-9dad-11d1-80b4-00c04fd430c8'::uuid$$; \
    CREATE FUNCTION uuid_ns_url() RETURNS uuid LANGUAGE sql IMMUTABLE PARALLEL SAFE \
        AS $$SELECT '6ba7b811-9dad-11d1-80b4-00c04fd430c8'::uuid$$; \
    CREATE FUNCTION uuid_ns_oid() RETURNS uuid LANGUAGE sql IMMUTABLE PARALLEL SAFE \
        AS $$SELECT '6ba7b812-9dad-11d1-80b4-00c04fd430c8'::uuid$$; \
    CREATE FUNCTION uuid_ns_x500() RETURNS uuid LANGUAGE sql IMMUTABLE PARALLEL SAFE \
        AS $$SELECT '6ba7b814-9dad-11d1-80b4-00c04fd430c8'::uuid$$;";

/// The functions every read of the transaction clock becomes ([`pat::clock_reads`]), one per type a
/// read can have, each the instant the setting `sqleq.clock` holds -- the one a trial sets -- as
/// Postgres derives that type from the transaction's start in the UTC session.
const CLOCK_FUNCTIONS: &str = "\
    CREATE SCHEMA sqleq_clock; \
    CREATE FUNCTION sqleq_clock.now() RETURNS timestamptz LANGUAGE sql STABLE PARALLEL SAFE \
        AS $$SELECT current_setting('sqleq.clock')::timestamptz$$; \
    CREATE FUNCTION sqleq_clock.local_ts() RETURNS timestamp LANGUAGE sql STABLE PARALLEL SAFE \
        AS $$SELECT sqleq_clock.now()::timestamp$$; \
    CREATE FUNCTION sqleq_clock.today() RETURNS date LANGUAGE sql STABLE PARALLEL SAFE \
        AS $$SELECT sqleq_clock.now()::date$$; \
    CREATE FUNCTION sqleq_clock.time_tz() RETURNS timetz LANGUAGE sql STABLE PARALLEL SAFE \
        AS $$SELECT sqleq_clock.now()::timetz$$; \
    CREATE FUNCTION sqleq_clock.local_time() RETURNS time LANGUAGE sql STABLE PARALLEL SAFE \
        AS $$SELECT sqleq_clock.now()::time$$;";

/// The template every run copies: an initialised cluster whose `sqleq_tmpl` database carries
/// [`EXTENSIONS`]. Built once per Postgres version, under `$SQLEQ_PG_CACHE` (default
/// `~/.cache/sqleq`), and renamed into place whole, so two processes building it at once each get a
/// complete one.
fn template(bin: &Path, ver: &str) -> Result<PathBuf, String> {
    let cache = match std::env::var_os("SQLEQ_PG_CACHE") {
        Some(d) => PathBuf::from(d),
        None => std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .ok_or("no cache directory: set SQLEQ_PG_CACHE")?
            .join("sqleq"),
    };
    // Keyed by where the binaries are too: two builds of one version can be configured differently.
    let place = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf()).hash(&mut h);
        h.finish()
    };
    let dir = cache.join(format!("pg-template-{ver}-v{TEMPLATE_FORMAT}-{place:016x}"));
    if dir.join("PG_VERSION").is_file() {
        return Ok(dir);
    }
    std::fs::create_dir_all(&cache).map_err(|e| format!("cannot create {}: {e}", cache.display()))?;
    let work = fresh_dir(RUN_PREFIX)?;
    let built = (|| {
        let data = work.join("data");
        run(Command::new(bin.join("initdb")).arg("-D").arg(&data).args([
            "--locale=C",
            "--encoding=UTF8",
            "-A",
            "trust",
            "-U",
            "postgres",
            "--no-sync",
            "-N",
        ]))?;
        {
            let _pm = Postmaster::start(bin, &data, &work, 8)?;
            let mut admin = Client::connect(&conninfo(&work, "postgres"), NoTls).map_err(|e| e.to_string())?;
            admin
                .batch_execute("CREATE DATABASE sqleq_tmpl")
                .map_err(|e| msg(&e))?;
            let mut t = Client::connect(&conninfo(&work, "sqleq_tmpl"), NoTls).map_err(|e| e.to_string())?;
            for ext in EXTENSIONS {
                // An install that lacks one leaves only the pairs that use it without a schema.
                let _ = t.batch_execute(&format!("CREATE EXTENSION IF NOT EXISTS \"{ext}\""));
            }
            let has_uuid = t
                .query_one("SELECT to_regprocedure('uuid_generate_v4()') IS NOT NULL", &[])
                .map(|r| r.get::<_, bool>(0))
                .unwrap_or(true);
            if !has_uuid {
                t.batch_execute(UUID_OSSP_CORE).map_err(|e| msg(&e))?;
            }
            t.batch_execute(&format!("SET sqleq.clock = '{}'; {CLOCK_FUNCTIONS}", clocks()[1]))
                .map_err(|e| msg(&e))?;
        }
        let staged = cache.join(format!(
            "{}.{}",
            dir.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&staged);
        std::fs::rename(&data, &staged).or_else(|_| copy_dir(&data, &staged)).map_err(|e| e.to_string())?;
        if std::fs::rename(&staged, &dir).is_err() {
            // Another process put a template in place first; theirs serves as well as ours.
            let _ = std::fs::remove_dir_all(&staged);
        }
        Ok::<(), String>(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    built.map(|()| dir)
}

/// A private cluster for one run: a copy of the cached template cluster in a fresh temp directory,
/// served on a unix socket there, with one database per worker. Dropping it stops the server and
/// removes the directory.
pub struct Server {
    root: PathBuf,
    postmaster: Option<Postmaster>,
    version: String,
}

impl Server {
    pub fn start(workers: usize) -> Result<Server, String> {
        let bin = bin_dir()?;
        let version = version(&bin)?;
        sweep_stale_runs();
        let tmpl = template(&bin, &version)?;
        let root = fresh_dir(RUN_PREFIX)?;
        let mut server = Server {
            root: root.clone(),
            postmaster: None,
            version,
        };
        copy_dir(&tmpl, &root.join("data")).map_err(|e| format!("cannot copy the template: {e}"))?;
        // Postgres refuses a data directory others can read.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(root.join("data"), std::fs::Permissions::from_mode(0o700));
        }
        server.postmaster = Some(Postmaster::start(&bin, &root.join("data"), &root, workers + 4)?);
        let mut admin = server.connect("postgres")?;
        for i in 0..workers {
            admin
                .batch_execute(&format!("CREATE DATABASE sqleq_w{i} TEMPLATE sqleq_tmpl"))
                .map_err(|e| msg(&e))?;
        }
        Ok(server)
    }

    /// The server's version, as `postgres --version` reports it (`17.11`).
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The database of worker `i`.
    pub fn worker(&self, i: usize) -> Result<Client, String> {
        self.connect(&format!("sqleq_w{i}"))
    }

    fn connect(&self, db: &str) -> Result<Client, String> {
        let mut c = Client::connect(&conninfo(&self.root, db), NoTls).map_err(|e| e.to_string())?;
        // A clock outside any trial -- preparing a statement, running the DDL -- reads the middle one.
        c.batch_execute(&format!(
            "SET statement_timeout = '10s'; SET lock_timeout = '2s'; SET sqleq.clock = '{}'",
            clocks()[1]
        ))
        .map_err(|e| msg(&e))?;
        Ok(c)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.postmaster.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The first line of a Postgres error, bounded like DuckDB's.
fn msg(e: &postgres::Error) -> String {
    let s = match e.as_db_error() {
        Some(d) => d.message().to_string(),
        None => e.to_string(),
    };
    s.lines().next().unwrap_or("").chars().take(240).collect()
}

/// What one pair cost, beside its verdict.
#[derive(Clone, Debug, Default)]
pub struct Timing {
    pub ddl_ms: f64,
    pub trial_ms: Vec<f64>,
    pub rows_tried: usize,
    pub rows_kept: usize,
    /// Types nothing declared, stood in for by a `text` domain.
    pub stand_ins: usize,
    /// Column defaults dropped because they call a function nothing declared.
    pub dropped_defaults: usize,
    /// Placeholders whose value domain came from the type Postgres inferred for them.
    pub typed_params: usize,
    /// Trials whose bags differed as text and agreed under `=`.
    pub eq_agreed: usize,
    /// Trials skipped because the bags could not be compared under `=`.
    pub uncomparable: usize,
    pub round_trips: usize,
}

pub struct Outcome {
    pub verdict: Verdict,
    pub timing: Timing,
}

impl Timing {
    /// What the verdict rests on beyond the DDL as written, if anything: a type read as text loses an
    /// enum's order and labels, and a dropped default changes what an INSERT omitting its column stores.
    pub fn caveat(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.stand_ins > 0 {
            parts.push(format!("{} undeclared types read as text", self.stand_ins));
        }
        if self.dropped_defaults > 0 {
            parts.push(format!(
                "{} defaults calling undeclared functions dropped",
                self.dropped_defaults
            ));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

/// A client that counts its round trips.
struct Db<'c> {
    c: &'c mut Client,
    trips: usize,
}

impl Db<'_> {
    fn exec(&mut self, sql: &str) -> Result<(), String> {
        self.trips += 1;
        self.c.batch_execute(sql).map_err(|e| msg(&e))
    }

    fn simple(&mut self, sql: &str) -> Result<Vec<SimpleQueryMessage>, String> {
        self.trips += 1;
        self.c.simple_query(sql).map_err(|e| msg(&e))
    }

    /// Run `sql` under a savepoint of its own, so a failure leaves the transaction usable.
    fn guarded<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        self.exec("SAVEPOINT g")?;
        match f(self) {
            Ok(v) => {
                self.exec("RELEASE SAVEPOINT g")?;
                Ok(v)
            }
            Err(e) => {
                self.exec("ROLLBACK TO SAVEPOINT g")?;
                Err(e)
            }
        }
    }
}

fn qi(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn ql(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A value as an untyped Postgres literal: Postgres resolves it to the type its context needs, as it
/// does an unknown-typed parameter, so one rendering serves an INSERT into any column type and a
/// placeholder in any position.
fn pg_lit(v: &Val) -> String {
    match v {
        Val::Null => "NULL".to_string(),
        Val::List(vs) => ql(&array_text(vs)),
        other => ql(&scalar_text(other)),
    }
}

fn scalar_text(v: &Val) -> String {
    match v {
        Val::Null => String::new(),
        Val::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Val::Int(i) => i.to_string(),
        Val::Dbl(d) => format!("{d:?}"),
        Val::Date(s) | Val::Ts(s) | Val::Str(s) | Val::Uuid(s) => s.clone(),
        Val::List(vs) => array_text(vs),
    }
}

/// A one-dimensional array in Postgres's text form, every element quoted.
fn array_text(vs: &[Val]) -> String {
    let elems: Vec<String> = vs
        .iter()
        .map(|v| match v {
            Val::Null => "NULL".to_string(),
            other => format!(
                "\"{}\"",
                scalar_text(other).replace('\\', "\\\\").replace('"', "\\\"")
            ),
        })
        .collect();
    format!("{{{}}}", elems.join(","))
}

/// Substitute each placeholder with its bound value: cast to the type the side's parameter has, which
/// is how a parameter of that type behaves, or as an untyped literal where no type is known.
fn substitute(
    sql: &str,
    ps: &[pat::Placeholder],
    binds: &HashMap<u32, Val>,
    types: &BTreeMap<u32, String>,
) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut last = 0;
    for p in ps {
        out.push_str(&sql[last..p.start]);
        let lit = binds.get(&p.n).map(pg_lit).unwrap_or_else(|| "NULL".to_string());
        match types.get(&p.n) {
            Some(t) => out.push_str(&format!("CAST({lit} AS {t})")),
            None => out.push_str(&lit),
        }
        last = p.end;
    }
    out.push_str(&sql[last..]);
    out
}

static QUAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:"([^"]+)"|([A-Za-z_][A-Za-z0-9_$]*))\s*\.\s*(?:"[^"]+"|[A-Za-z_])"#).unwrap()
});
static CREATE_TABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(CREATE\s+(?:UNLOGGED\s+)?TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?)("[^".]+"|[A-Za-z_][A-Za-z0-9_$]*)(\s*\()"#,
    )
    .unwrap()
});
static MISSING_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^type "([^"]+)" does not exist"#).unwrap());
/// An ICU collation: a provider named `icu`, a predefined `...-x-icu` collation, or an `icu_`
/// function.
static ICU: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)provider\s*=\s*['"]?icu\b|-x-icu\b|\bicu_[a-z]"#).unwrap()
});
static MISSING_FUNCTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^function ([A-Za-z0-9_$.]+)\(\) does not exist").unwrap());

/// `ddl` without each `DEFAULT name()` that calls the zero-argument function `name`, if it has any.
fn drop_defaults(ddl: &str, name: &str) -> Option<String> {
    let parts: Vec<String> = name
        .split('.')
        .map(|p| format!(r#""?{}"?"#, regex::escape(p)))
        .collect();
    // `DEFAULT f()`, `DEFAULT (f())`, either with a cast: `DEFAULT (f())::text`.
    let call = format!(r"{}\s*\(\s*\)", parts.join(r"\s*\.\s*"));
    let cast = r#"(?:\s*::\s*"?[A-Za-z_][A-Za-z0-9_$.]*"?(?:\s*\(\s*\d+(?:\s*,\s*\d+)?\s*\))?(?:\s*\[\])?)?"#;
    let re = Regex::new(&format!(r"(?i)\s+DEFAULT\s+(?:\(\s*{call}\s*\)|{call}){cast}")).ok()?;
    re.is_match(ddl).then(|| re.replace_all(ddl, "").into_owned())
}

const SKIP_SCHEMAS: &[&str] = &["pg_catalog", "public", "information_schema", "excluded", "new", "old"];

/// Every name that qualifies something in `text`: a superset of the schemas it names, and creating
/// an empty schema constrains nothing.
fn qualifiers(text: &str) -> BTreeSet<String> {
    QUAL.captures_iter(text)
        .filter_map(|c| c.get(1).or_else(|| c.get(2)).map(|m| m.as_str().to_string()))
        .filter(|s| !SKIP_SCHEMAS.contains(&s.to_lowercase().as_str()))
        .collect()
}

/// Create an unqualified `CREATE TABLE t` in the one schema `refs` names it by (`CREATE INDEX ... ON
/// s.t`), as captured DDL drops the schema from the table but not from its indexes.
fn place_tables(ddl: &str, refs: &str) -> String {
    CREATE_TABLE
        .replace_all(ddl, |m: &regex::Captures| {
            let name = &m[2];
            let bare = name.trim_matches('"');
            let re = Regex::new(&format!(
                r#"(?:"([^"]+)"|([A-Za-z_][A-Za-z0-9_$]*))\s*\.\s*"?{}"?(?:[^A-Za-z0-9_$]|$)"#,
                regex::escape(bare)
            ))
            .unwrap();
            let schemas: BTreeSet<String> = re
                .captures_iter(refs)
                .filter_map(|c| c.get(1).or_else(|| c.get(2)).map(|m| m.as_str().to_string()))
                .filter(|s| !SKIP_SCHEMAS.contains(&s.to_lowercase().as_str()))
                .collect();
            if schemas.len() == 1 {
                let s = schemas.into_iter().next().unwrap();
                format!("{}{}.{}{}", &m[1], qi(&s), name, &m[3])
            } else {
                m[0].to_string()
            }
        })
        .into_owned()
}

/// Where one schema table's rows go: its relation, and for each of the generator's columns the
/// column it fills (none for a column Postgres computes itself or does not have).
struct Target {
    reg: String,
    nsp: String,
    cols: Vec<Option<String>>,
    overriding: bool,
    /// Every column's type, in table order: what `SELECT *` returns.
    types: Vec<String>,
    /// For each of the generator's columns, its declared type.
    col_types: Vec<Option<String>>,
}

fn target(db: &mut Db, key: &str, gen_cols: &[String]) -> Result<Target, String> {
    let (nsp, name) = match key.rsplit_once('.') {
        Some((s, n)) => (Some(s.to_string()), n.to_string()),
        None => (None, key.to_string()),
    };
    db.trips += 1;
    let rows = db
        .c
        .query(
            "SELECT c.oid::regclass::text, a.attname::text, a.attgenerated::text, \
                    a.attidentity::text, format_type(a.atttypid, a.atttypmod), c.oid::int8, \
                    n.nspname::text \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
             WHERE lower(c.relname) = $1 AND c.relkind IN ('r', 'p') \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
               AND ($2::text IS NULL OR lower(n.nspname) = $2) \
             ORDER BY pg_table_is_visible(c.oid) DESC, c.oid, a.attnum",
            &[&name, &nsp],
        )
        .map_err(|e| msg(&e))?;
    let first = rows.first().ok_or(format!("no relation for table {key}"))?;
    let oid: i64 = first.get(5);
    let reg: String = first.get(0);
    let nsp: String = first.get(6);
    let mut by_name: HashMap<String, (String, bool, String)> = HashMap::new();
    let mut overriding = false;
    let mut types = Vec::new();
    for r in rows.iter().filter(|r| r.get::<_, i64>(5) == oid) {
        let att: String = r.get(1);
        let generated = !r.get::<_, String>(2).is_empty();
        if r.get::<_, String>(3) == "a" {
            overriding = true;
        }
        let ty: String = r.get(4);
        types.push(ty.clone());
        by_name.insert(att.to_lowercase(), (att, generated, ty));
    }
    let cols = gen_cols
        .iter()
        .map(|c| match by_name.get(&c.to_lowercase()) {
            Some((att, false, _)) => Some(qi(att)),
            _ => None,
        })
        .collect();
    let col_types = gen_cols
        .iter()
        .map(|c| by_name.get(&c.to_lowercase()).map(|(_, _, t)| t.clone()))
        .collect();
    Ok(Target {
        reg,
        nsp,
        cols,
        overriding,
        types,
        col_types,
    })
}

fn insert_sql(t: &Target, rows: &[&Vec<Val>]) -> Option<String> {
    let idx: Vec<usize> = (0..t.cols.len()).filter(|&j| t.cols[j].is_some()).collect();
    if idx.is_empty() || rows.is_empty() {
        return None;
    }
    let cols = idx
        .iter()
        .map(|&j| t.cols[j].clone().unwrap())
        .collect::<Vec<_>>()
        .join(", ");
    let values = rows
        .iter()
        .map(|r| {
            format!(
                "({})",
                idx.iter().map(|&j| pg_lit(&r[j])).collect::<Vec<_>>().join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let over = if t.overriding {
        " OVERRIDING SYSTEM VALUE"
    } else {
        ""
    };
    Some(format!("INSERT INTO {} ({cols}){over} VALUES {values}", t.reg))
}

/// The generator's domain for a Postgres type named as `format_type` names it, and whether the type
/// is an array of it.
fn vtype_of(name: &str) -> Option<(VType, bool)> {
    if let Some(elem) = name.strip_suffix("[]") {
        return vtype_of(elem).filter(|(_, arr)| !arr).map(|(v, _)| (v, true));
    }
    let base = name.split('(').next().unwrap_or(name).trim();
    Some((
        match base {
            "smallint" | "integer" | "bigint" | "oid" => VType::Integer,
            "real" | "double precision" => VType::Double,
            "numeric" => VType::Decimal(38, 18),
            "boolean" => VType::Boolean,
            "date" => VType::Date,
            "timestamp without time zone" => VType::Timestamp,
            "timestamp with time zone" => VType::TimestampTz,
            "text" | "character varying" | "character" | "name" | "citext" => VType::Varchar,
            "uuid" => VType::Uuid,
            "json" | "jsonb" => VType::Json,
            "interval" => VType::Interval,
            _ => return None,
        },
        false,
    ))
}

/// Whether a value of the generator's domain `l` is a valid input for a placeholder Postgres types `p`.
fn compatible(p: VType, l: VType) -> bool {
    p.same_domain(l)
        || p == VType::Varchar
        || (matches!(p, VType::Double | VType::Decimal(..)) && l == VType::Integer)
        || (matches!(p, VType::Timestamp | VType::TimestampTz) && l == VType::Date)
}

static PREPARED: AtomicUsize = AtomicUsize::new(0);

/// How many statements `sql` holds, by its top-level semicolons; 0 if it does not lex.
fn statements(sql: &str) -> usize {
    match crate::lex::significant(sql) {
        Some(toks) => {
            let semis = toks
                .iter()
                .filter(|t| matches!(t.token, sqlparser::tokenizer::Token::SemiColon))
                .count();
            let trailing = matches!(
                toks.last().map(|t| &t.token),
                Some(sqlparser::tokenizer::Token::SemiColon)
            );
            semis + 1 - usize::from(trailing)
        }
        None => 0,
    }
}

/// The type each of `sql`'s placeholders has once Postgres prepares it with `declared` (unknown where
/// absent), or nothing if it cannot. A number the side skips is declared `text`, since Postgres
/// cannot infer a parameter nothing uses.
fn side_types(
    db: &mut Db,
    sql: &str,
    ph: &[pat::Placeholder],
    declared: &BTreeMap<u32, String>,
) -> Option<BTreeMap<u32, String>> {
    let used: BTreeSet<u32> = ph.iter().map(|p| p.n).collect();
    let nmax = used.iter().max().copied().unwrap_or(0);
    let decl = if nmax == 0 {
        String::new()
    } else {
        let ts: Vec<String> = (1..=nmax)
            .map(|n| match declared.get(&n) {
                Some(t) => t.clone(),
                None if used.contains(&n) => "unknown".to_string(),
                None => "text".to_string(),
            })
            .collect();
        format!(" ({})", ts.join(", "))
    };
    // `PREPARE` takes one statement; given several, it would run the rest. And it is not
    // transactional: a prepared statement outlives the rollback that undoes everything else, so a
    // name is used once per process, never once per pair.
    if statements(sql) != 1 {
        return None;
    }
    let name = format!("sqleq_p{}", PREPARED.fetch_add(1, Ordering::Relaxed));
    let body = sql.trim_end().trim_end_matches(';').to_string();
    db.guarded(|db| {
        db.exec(&format!("PREPARE {name}{decl} AS {body}"))?;
        db.trips += 1;
        let rows = db
            .c
            .query(
                "SELECT t::text FROM pg_prepared_statements, \
                        unnest(parameter_types) WITH ORDINALITY AS u(t, i) \
                 WHERE name = $1 ORDER BY i",
                &[&name],
            )
            .map_err(|e| msg(&e))?;
        db.exec(&format!("DEALLOCATE {name}"))?;
        Ok(rows
            .iter()
            .enumerate()
            .map(|(i, r)| (i as u32 + 1, r.get::<_, String>(0)))
            .filter(|(n, _)| used.contains(n))
            .collect())
    })
    .ok()
}

/// One observed bag: a label (the returned rows, or a table), its column types when known, and its
/// rows as Postgres's text.
#[derive(Clone, Debug, PartialEq)]
struct Bag {
    label: String,
    types: Option<Vec<String>>,
    rows: Vec<Vec<Option<String>>>,
}

impl Bag {
    fn keys(&self) -> Vec<String> {
        let mut k: Vec<String> = self
            .rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|c| c.as_deref().unwrap_or("\u{0}"))
                    .collect::<Vec<_>>()
                    .join("\u{1}")
            })
            .collect();
        k.sort();
        k
    }
}

fn row_of(r: &postgres::SimpleQueryRow) -> Vec<Option<String>> {
    (0..r.len()).map(|i| r.get(i).map(str::to_string)).collect()
}

/// Run one side under a savepoint of its own and return what it shows: the rows of a query, or a
/// mutation's returned rows and every target table's rows afterwards.
fn run_side(
    db: &mut Db,
    stmt: &str,
    is_query: bool,
    returning: bool,
    reset: &str,
    targets: &[(String, Target)],
) -> Result<Vec<Bag>, String> {
    let mut sql = String::from("SAVEPOINT side;\n");
    let mut skip = 1; // statements before the side's own
    if !is_query && !reset.is_empty() {
        sql.push_str(reset);
        sql.push_str(";\n");
        skip += 1;
    }
    sql.push_str(stmt.trim_end().trim_end_matches(';'));
    sql.push_str("\n;\n");
    let mut labels: Vec<String> = Vec::new();
    if is_query || returning {
        labels.push("\u{1}returning".to_string());
    } else {
        labels.push(String::new()); // the statement's own completion, no rows kept
    }
    if !is_query {
        for (key, t) in targets {
            sql.push_str(&format!("SELECT * FROM {};\n", t.reg));
            labels.push(key.clone());
        }
    }
    sql.push_str("ROLLBACK TO SAVEPOINT side");
    let msgs = match db.simple(&sql) {
        Ok(m) => m,
        Err(e) => {
            // A syntax error rejects the whole batch before its savepoint is taken, so this can
            // fail too; the trial's own rollback, which follows, recovers either way. The error the
            // side raised is the one to report.
            let _ = db.exec("ROLLBACK TO SAVEPOINT side");
            return Err(e);
        }
    };
    // One segment of rows per statement, closed by its completion. The layout is known from both
    // ends -- the savepoint (and sequence reset) first, then the side's own statements, then one
    // `SELECT *` per target table, then the rollback -- so a mutation side may be several statements.
    let mut segments: Vec<Vec<Vec<Option<String>>>> = vec![Vec::new()];
    for m in &msgs {
        match m {
            SimpleQueryMessage::Row(r) => segments.last_mut().unwrap().push(row_of(r)),
            SimpleQueryMessage::CommandComplete(_) => segments.push(Vec::new()),
            _ => {}
        }
    }
    segments.pop(); // the empty segment after the last completion
    let tables = labels.len() - 1;
    let own = segments.len().saturating_sub(skip + tables + 1);
    if own == 0 || (is_query && own != 1) {
        return Err(format!("side is not one statement ({} completions)", segments.len()));
    }
    let mut bags: Vec<Bag> = Vec::new();
    if is_query || returning {
        bags.push(Bag {
            label: labels[0].clone(),
            types: None,
            rows: segments[skip..skip + own].concat(),
        });
    }
    for (i, l) in labels[1..].iter().enumerate() {
        bags.push(Bag {
            label: l.clone(),
            types: None,
            rows: std::mem::take(&mut segments[skip + own + i]),
        });
    }
    for b in bags.iter_mut() {
        if let Some((_, t)) = targets.iter().find(|(k, _)| *k == b.label) {
            b.types = Some(t.types.clone());
        }
    }
    Ok(bags)
}

fn finish(db: &mut Db) -> Result<(), String> {
    db.exec("ROLLBACK TO SAVEPOINT trial")
}

/// The result column types of `stmt`, by preparing it.
fn result_types(db: &mut Db, stmt: &str) -> Option<Vec<String>> {
    let st = db
        .guarded(|db| {
            db.trips += 1;
            db.c.prepare(stmt).map_err(|e| msg(&e))
        })
        .ok()?;
    let oids: Vec<u32> = st.columns().iter().map(|c| c.type_().oid()).collect();
    if oids.is_empty() {
        return Some(Vec::new());
    }
    db.trips += 1;
    let rows = db
        .c
        .query(
            "SELECT format_type(o, NULL) FROM unnest($1::oid[]) WITH ORDINALITY AS u(o, i) ORDER BY i",
            &[&oids],
        )
        .ok()?;
    Some(rows.iter().map(|r| r.get::<_, String>(0)).collect())
}

/// A type Postgres has no `=` for, as its error names it.
static NO_EQUALITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"could not identify an equality operator for type (.+)$").unwrap());

/// Whether two bags of one size hold the same rows: `Ok(true)` same, `Ok(false)` different, `Err`
/// when they cannot be compared.
///
/// Rows are the same when their values are equal under Postgres `=`. A type Postgres has no `=` for
/// (`json`, `xml`, `point`, ...) is compared by its text instead -- what `json` keeps verbatim and
/// what a client receives -- while every other column is still compared under `=`. Which types
/// those are is Postgres's to say: each one its error names is read as `text` (an array of it as
/// `text[]`) and the comparison is asked again. A record holding such a type cannot be read as
/// text that way, and stays uncomparable.
fn same_under_eq(db: &mut Db, a: &Bag, ta: &[String], b: &Bag, tb: &[String]) -> Result<bool, String> {
    let (mut ta, mut tb) = (ta.to_vec(), tb.to_vec());
    loop {
        match compare_under_eq(db, a, &ta, b, &tb) {
            Err(e) => {
                let Some(t) = NO_EQUALITY.captures(&e).map(|c| c[1].trim().to_string()) else {
                    return Err(e);
                };
                let changed = as_text(&mut ta, &t) | as_text(&mut tb, &t);
                if !changed {
                    return Err(e);
                }
            }
            done => return done,
        }
    }
}

/// Read every column of type `t`, or an array of it, as text. Whether any was.
fn as_text(types: &mut [String], t: &str) -> bool {
    let mut changed = false;
    for ty in types.iter_mut() {
        if ty == t {
            *ty = "text".to_string();
            changed = true;
        } else if ty.strip_suffix("[]") == Some(t) {
            *ty = "text[]".to_string();
            changed = true;
        }
    }
    changed
}

/// [`same_under_eq`] with the types as given: `Err` also when one of them has no `=`.
fn compare_under_eq(
    db: &mut Db,
    a: &Bag,
    ta: &[String],
    b: &Bag,
    tb: &[String],
) -> Result<bool, String> {
    if ta.is_empty() || tb.is_empty() {
        return Ok(true); // no columns: equal sizes are equal bags
    }
    let values = |bag: &Bag, types: &[String]| -> String {
        let rows = bag
            .rows
            .iter()
            .map(|r| {
                let cells = r
                    .iter()
                    .zip(types)
                    .map(|(c, t)| match c {
                        Some(s) => format!("CAST({} AS {t})", ql(s)),
                        None => format!("CAST(NULL AS {t})"),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({cells})")
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("(VALUES {rows})")
    };
    let va = values(a, ta);
    let vb = values(b, tb);
    let sql = format!(
        "SELECT ((SELECT count(*) FROM ({va} EXCEPT ALL {vb}) x) + \
                 (SELECT count(*) FROM ({vb} EXCEPT ALL {va}) y))::text"
    );
    // Bags whose rows have different widths, or no common type in some column, hold different rows.
    let msgs = match db.guarded(|db| db.simple(&sql)) {
        Ok(m) => m,
        Err(e)
            if e.contains("same number of columns")
                || e.contains("cannot be matched")
                || e.contains("could not convert type") =>
        {
            return Ok(false)
        }
        Err(e) => return Err(e),
    };
    for m in msgs {
        if let SimpleQueryMessage::Row(r) = m {
            return Ok(r.get(0) == Some("0"));
        }
    }
    Err("no comparison result".to_string())
}

/// Render a counterexample: the clock, where the pair reads it, the bound params and the rows Postgres
/// accepted.
fn describe(binds: &HashMap<u32, Val>, kept: &RowData, clock: Option<&str>) -> String {
    let mut ps: Vec<(u32, &Val)> = binds.iter().map(|(k, v)| (*k, v)).collect();
    ps.sort_by_key(|(k, _)| *k);
    let params = ps
        .iter()
        .map(|(k, v)| format!("${k}={}", pg_lit(v)))
        .collect::<Vec<_>>()
        .join(", ");
    let tables = kept
        .iter()
        .map(|(t, rows)| {
            let rs = rows
                .iter()
                .map(|r| format!("({})", r.iter().map(pg_lit).collect::<Vec<_>>().join(",")))
                .collect::<Vec<_>>()
                .join("; ");
            format!("{t}=[{rs}]")
        })
        .collect::<Vec<_>>()
        .join("  ");
    let mut parts = Vec::new();
    if let Some(c) = clock {
        parts.push(format!("clock: {c}"));
    }
    if !params.is_empty() {
        parts.push(format!("params: {params}"));
    }
    parts.push(tables);
    parts.join("  |  ")
}

/// Test one pair on Postgres. The connection must not be inside a transaction.
pub fn test_pair_pg(client: &mut Client, a: &str, b: &str, ddl: &str, cfg: Config) -> Outcome {
    let mut db = Db { c: client, trips: 0 };
    let mut timing = Timing::default();
    let verdict = match db.exec("BEGIN") {
        Ok(()) => run_pair(&mut db, a, b, ddl, cfg, &mut timing),
        Err(e) => Verdict::Error(e),
    };
    let _ = db.exec("ROLLBACK");
    timing.round_trips = db.trips;
    Outcome { verdict, timing }
}

fn run_pair(db: &mut Db, a: &str, b: &str, ddl: &str, cfg: Config, timing: &mut Timing) -> Verdict {
    // The clock is an input of each trial, like a placeholder: every read of it, in the DDL's
    // defaults and in both statements, reads the instant the trial sets (see [`clocks`]), as reads
    // of one transaction's clock agree.
    let (a, b, ddl) = (pat::clock_reads(a), pat::clock_reads(b), pat::clock_reads(ddl));
    let (a, b, ddl) = (a.as_str(), b.as_str(), ddl.as_str());
    let clocked = [a, b, ddl].iter().any(|s| s.contains(pat::CLOCK_SCHEMA));
    // A function body reads the real clock, not the trial's: a trigger that stamps a row would
    // stamp it with an instant a statement's own read never sees.
    if clocked && [a, b, ddl].iter().any(|s| pat::reads_clock_in_body(s)) {
        return Verdict::NotComparable("clock: a function body reads the clock".to_string());
    }
    if pat::has_explain(a, b) {
        return Verdict::NotComparable("explain".to_string());
    }
    if pat::has_hard_nondet(a, b) {
        return Verdict::NondetSkip;
    }
    // Not every PostgreSQL 17 is built with ICU -- the one the build fetches is not -- so a pair that
    // needs an ICU collation would get a verdict on one machine and an error on another. It gets
    // neither, on any.
    if [a, b, ddl].iter().any(|s| ICU.is_match(s)) {
        return Verdict::NotComparable(
            "an ICU collation, which not every PostgreSQL build has".to_string(),
        );
    }
    let schema = parse_schema(ddl);
    if schema.is_empty() {
        return Verdict::NoSchema;
    }
    let forms = table_forms(a, b, &schema);
    if forms.is_empty() {
        return Verdict::NoTables;
    }
    let finals: Vec<String> = forms.keys().cloned().collect();
    let (ph_a, ph_b) = match (pat::placeholders(a), pat::placeholders(b)) {
        (Ok(pa), Ok(pb)) => (pa, pb),
        (Err(e), _) | (_, Err(e)) => return Verdict::Error(e),
    };
    let misaligned = pat::misalignment(a, b);

    // --- the parameter analysis, as `test_pair` does it ---
    let mut coltype: HashSet<String> = HashSet::new();
    let mut colloc: HashMap<String, (String, usize, VType, bool)> = HashMap::new();
    for t in &finals {
        for (j, c) in schema[t].cols.iter().enumerate() {
            coltype.insert(c.name.clone());
            colloc
                .entry(c.name.clone())
                .or_insert((t.clone(), j, c.vt, c.array));
        }
    }
    let coltypes: HashMap<String, VType> = colloc
        .iter()
        .filter(|(_, (_, _, _, array))| !array)
        .map(|(name, (_, _, vt, _))| (name.clone(), *vt))
        .collect();
    let pneed = typing::param_needs(a, b, &coltypes);
    let pcol = pat::param_cols(a, b, &coltype);
    let pcast = pat::param_casts(a, b);
    let arraycols: HashSet<String> = colloc
        .iter()
        .filter(|(_, (_, _, _, arr))| *arr)
        .map(|(n, _)| n.clone())
        .collect();
    let parray = pat::array_params(a, b, &arraycols);
    let pnums: BTreeSet<u32> = ph_a.iter().chain(&ph_b).map(|p| p.n).collect();

    let cuts: Vec<limits::Cut> = limits::cuts(a, &schema)
        .into_iter()
        .chain(limits::cuts(b, &schema))
        .collect();
    let mut counted: BTreeMap<u32, BTreeSet<Count>> = BTreeMap::new();
    for c in &cuts {
        for (n, kind) in &c.params {
            counted.entry(*n).or_default().insert(*kind);
        }
    }
    let neutral: HashMap<u32, Val> = counted
        .iter()
        .filter(|(n, kinds)| !pcol.contains_key(n) && kinds.len() == 1)
        .map(|(n, kinds)| {
            let v = match kinds.first() {
                Some(Count::Offset) => Val::Int(0),
                _ => Val::Int(1_000_000_000),
            };
            (*n, v)
        })
        .collect();
    let open = |c: &limits::Cut| {
        !c.total && (c.fixed || c.params.iter().any(|(n, _)| !neutral.contains_key(n)))
    };
    let cut_nondet = cuts.iter().any(open);
    let choice = limits::choices(a, &schema).max(limits::choices(b, &schema));
    // A cut under a level that can tell its tied rows apart leaves the cardinality open too, as a
    // `DISTINCT ON` there does ([`limits::Cut::counted`]).
    if choice == limits::Choice::Unbounded || cuts.iter().any(|c| !c.counted && open(c)) {
        return Verdict::NondetSkip;
    }
    // `array_agg` with no order: DuckDB's path sorts list cells; text cannot be sorted faithfully
    // here, so such a pair compares only cardinality.
    let unordered_agg = [a, b]
        .iter()
        .any(|s| s.to_lowercase().contains("array_agg"));
    let nondet = cut_nondet
        || choice == limits::Choice::Cardinality
        || pat::has_nondet_agg(a, b)
        || unordered_agg;

    let is_query_a = is_query(a);
    let is_query_b = is_query(b);
    if is_query_a != is_query_b {
        return Verdict::NotComparable(format!(
            "mixed-kind: {} vs {}",
            if is_query_a { "query" } else { "mutation" },
            if is_query_b { "query" } else { "mutation" },
        ));
    }
    let ret_a = pat::has_returning(a);
    let ret_b = pat::has_returning(b);
    if ret_a != ret_b {
        return Verdict::NotComparable(format!(
            "one-sided RETURNING: {} vs {}",
            if ret_a { "returning" } else { "none" },
            if ret_b { "returning" } else { "none" },
        ));
    }

    // --- the schema, verbatim ---
    let started = Instant::now();
    let mut schemas = qualifiers(ddl);
    for parts in forms.values().flatten() {
        if parts.len() >= 2 {
            schemas.insert(parts[parts.len() - 2].clone());
        }
    }
    schemas.retain(|s| !SKIP_SCHEMAS.contains(&s.to_lowercase().as_str()));
    // Placed by the DDL's own references, so the DDL runs as captured; the queries' spelling is
    // honoured below, by moving the table once it exists.
    let mut placed = place_tables(ddl, ddl);
    let mut pre = String::new();
    for s in &schemas {
        pre.push_str(&format!("CREATE SCHEMA IF NOT EXISTS {};\n", qi(s)));
    }
    if !schemas.is_empty() {
        pre.push_str(&format!(
            "SET LOCAL search_path = public, {};\n",
            schemas.iter().map(|s| qi(s)).collect::<Vec<_>>().join(", ")
        ));
    }
    if !pre.is_empty() {
        if let Err(e) = db.exec(&pre) {
            return Verdict::Error(format!("ddl: {e}"));
        }
    }
    let mut stubs: Vec<String> = Vec::new();
    loop {
        let mut sql = String::from("SAVEPOINT ddl;\n");
        for s in stubs.iter().filter(|s| !s.is_empty()) {
            sql.push_str(&format!("CREATE DOMAIN {s} AS text;\n"));
        }
        sql.push_str(&placed);
        match db.exec(&sql) {
            Ok(()) => break,
            Err(e) => {
                if db.exec("ROLLBACK TO SAVEPOINT ddl").is_err() {
                    return Verdict::Error(format!("ddl: {e}"));
                }
                let missing = MISSING_TYPE.captures(&e).map(|c| {
                    c[1].trim_end_matches("[]")
                        .split('.')
                        .map(qi)
                        .collect::<Vec<_>>()
                        .join(".")
                });
                // A column default that calls a function nothing declares is dropped, as DuckDB drops
                // every default: only an INSERT that omits the column can tell.
                let dropped = MISSING_FUNCTION
                    .captures(&e)
                    .and_then(|c| drop_defaults(&placed, &c[1]));
                match (missing, dropped) {
                    (Some(m), _) if stubs.len() < 25 && !stubs.contains(&m) => stubs.push(m),
                    (None, Some(d)) if stubs.len() < 25 => {
                        placed = d;
                        stubs.push(String::new());
                    }
                    _ => return Verdict::Error(format!("ddl: {e}")),
                }
            }
        }
    }
    timing.stand_ins = stubs.iter().filter(|s| !s.is_empty()).count();
    timing.dropped_defaults = stubs.iter().filter(|s| s.is_empty()).count();

    // A table the queries name by one schema, created in another, moves there: it is one table under
    // the spelling the queries use. Named by two schemas, it is two tables to the queries.
    let mut targets: Vec<(String, Target)> = Vec::new();
    for t in &finals {
        let names: Vec<String> = schema[t].cols.iter().map(|c| c.name.clone()).collect();
        let quals: BTreeSet<&String> = forms[t]
            .iter()
            .filter(|p| p.len() >= 2)
            .map(|p| &p[p.len() - 2])
            .collect();
        if quals.len() > 1 {
            return Verdict::NotComparable(format!("table {t} named in several schemas"));
        }
        let mut tg = match target(db, t, &names) {
            Ok(tg) => tg,
            Err(e) => return Verdict::Error(e),
        };
        if let Some(s) = quals.first() {
            if tg.nsp.to_lowercase() != **s {
                if let Err(e) = db.exec(&format!("ALTER TABLE {} SET SCHEMA {}", tg.reg, qi(s))) {
                    return Verdict::Error(format!("ddl: {e}"));
                }
                let key = format!("{s}.{}", t.rsplit('.').next().unwrap_or(t));
                tg = match target(db, &key, &names) {
                    Ok(tg) => tg,
                    Err(e) => return Verdict::Error(e),
                };
            }
        }
        targets.push((t.clone(), tg));
    }
    let mutates = !is_query_a || !is_query_b;
    let reset = if mutates {
        db.trips += 1;
        match db.c.query(
            "SELECT coalesce('DO $r$ BEGIN ' || string_agg(format('PERFORM setval(%L, %s, false);', \
                    c.oid::regclass::text, s.seqstart), ' ') || ' END $r$', '') \
             FROM pg_sequence s JOIN pg_class c ON c.oid = s.seqrelid",
            &[],
        ) {
            Ok(rows) => rows.first().map(|r| r.get::<_, String>(0)).unwrap_or_default(),
            Err(e) => return Verdict::Error(msg(&e)),
        }
    } else {
        String::new()
    };
    // A column default that calls a volatile function (`gen_random_uuid()`, `clock_timestamp()`, a
    // user function not declared otherwise) draws a value of its own on each side, so two inserts
    // that omit the column fill it differently whatever they are; only the number of rows each side
    // leaves is a fact about the statements. `nextval` is the exception: sequences are reset before
    // each side. Every function a default calls, built-in or not, is named in its stored expression
    // tree, as a `:funcid` or an operator's `:opfuncid`; `pg_depend` would miss the built-in ones,
    // on which no dependency is recorded.
    let volatile_default = mutates && {
        let regs: Vec<String> = targets.iter().map(|(_, t)| t.reg.clone()).collect();
        db.trips += 1;
        match db.c.query(
            "SELECT EXISTS (SELECT 1 FROM pg_attrdef ad \
               CROSS JOIN LATERAL regexp_matches(ad.adbin::text, ':(?:op)?funcid ([0-9]+)', 'g') AS m \
               JOIN pg_proc p ON p.oid = m[1]::oid \
              WHERE ad.adrelid = ANY (ARRAY(SELECT r::regclass::oid FROM unnest($1::text[]) AS r)) \
                AND p.provolatile = 'v' AND p.oid <> 'nextval(regclass)'::regprocedure)",
            &[&regs],
        ) {
            Ok(rows) => rows.first().map(|r| r.get::<_, bool>(0)).unwrap_or(true),
            Err(e) => return Verdict::Error(msg(&e)),
        }
    };
    let nondet = nondet || volatile_default;
    // What Postgres stored decides, not the text: a default that still reads the real clock (a
    // spelling the rewrite does not know) would give an insert an instant no statement reads.
    if clocked && mutates {
        let regs: Vec<String> = targets.iter().map(|(_, t)| t.reg.clone()).collect();
        db.trips += 1;
        match db.c.query(
            "SELECT EXISTS (SELECT 1 FROM pg_attrdef ad \
              WHERE ad.adrelid = ANY (ARRAY(SELECT r::regclass::oid FROM unnest($1::text[]) AS r)) \
                AND regexp_replace(pg_get_expr(ad.adbin, ad.adrelid), 'sqleq_clock\\.\\w+\\(\\)', '', 'g') \
                    ~* '\\m(now|statement_timestamp|transaction_timestamp)\\(|\\m(current_timestamp|current_date|current_time|localtimestamp|localtime)\\M|''(now|today|tomorrow|yesterday)''')",
            &[&regs],
        ) {
            Ok(rows) if rows.first().is_some_and(|r| r.get::<_, bool>(0)) => {
                return Verdict::NotComparable("clock: a column default reads the clock".to_string());
            }
            Ok(_) => {}
            Err(e) => return Verdict::Error(msg(&e)),
        }
    }
    let clocks = clocks();
    // Each side's parameter types: inferred by Postgres, or failing that, inferred with the
    // heuristics' column links declared as hints. The generator's domain for a placeholder is taken
    // only where both sides agree on it.
    let hints: BTreeMap<u32, String> = pnums
        .iter()
        .filter_map(|n| {
            let (t, idx, _, col_array) = colloc.get(pcol.get(n)?)?;
            let ty = targets.iter().find(|(k, _)| k == t)?.1.col_types[*idx].clone()?;
            Some((*n, if parray.contains(n) && !col_array { format!("{ty}[]") } else { ty }))
        })
        .collect();
    let infer = |db: &mut Db, sql: &str, ph: &[pat::Placeholder]| {
        side_types(db, sql, ph, &BTreeMap::new())
            .or_else(|| side_types(db, sql, ph, &hints))
            .unwrap_or_default()
    };
    let types_a = infer(db, a, &ph_a);
    let types_b = infer(db, b, &ph_b);
    // One `$N` is one value, of one type, in both statements. Where a side leaves a placeholder
    // untyped (`SELECT $2 AS x`, which Postgres cannot prepare without being told), the type the
    // other side gives it is the one the application bound; untyped, it would be read as text.
    let fill = |db: &mut Db,
                sql: &str,
                ph: &[pat::Placeholder],
                mine: &BTreeMap<u32, String>,
                other: &BTreeMap<u32, String>| {
        if ph.iter().all(|p| mine.contains_key(&p.n)) {
            return mine.clone();
        }
        let mut declared = hints.clone();
        declared.extend(other.iter().map(|(n, t)| (*n, t.clone())));
        declared.extend(mine.iter().map(|(n, t)| (*n, t.clone())));
        side_types(db, sql, ph, &declared).unwrap_or_else(|| mine.clone())
    };
    let types_b = fill(db, b, &ph_b, &types_b, &types_a);
    let types_a = fill(db, a, &ph_a, &types_a, &types_b);
    // An array on one side and a scalar on the other is not one value bound to both: the caller
    // pairs those placeholders some other way (a row of a VALUES list and an element of an array,
    // say), so no binding by index compares what the caller paired. Postgres would even run such a
    // pair -- an array assigns to a text column as its text -- and refute what it never meant.
    // Where Postgres cannot type a side, the casts that side writes itself still say which of its
    // placeholders are arrays.
    let shapes = |sql: &str, types: &BTreeMap<u32, String>| {
        let mut out = pat::param_cast_shapes(sql);
        out.extend(types.iter().map(|(n, t)| (*n, t.clone())));
        out
    };
    let (shapes_a, shapes_b) = (shapes(a, &types_a), shapes(b, &types_b));
    for n in &pnums {
        if let (Some(x), Some(y)) = (shapes_a.get(n), shapes_b.get(n)) {
            if x.ends_with(']') != y.ends_with(']') {
                return Verdict::NotComparable(format!(
                    "param-shape: ${n} is {x} on one side and {y} on the other"
                ));
            }
        }
    }
    // Postgres resolves a placeholder nothing constrains -- `SELECT $2 AS x` -- to `text`. That is a
    // default, not evidence: where the other side gives the same `$N` a specific type, that type is
    // the one the application bound, and the defaulted side is prepared again with it declared.
    // A side that casts the placeholder itself (`$1::text`) typed it on purpose, and keeps its type.
    let prefer = |db: &mut Db,
                  sql: &str,
                  ph: &[pat::Placeholder],
                  mine: BTreeMap<u32, String>,
                  other: &BTreeMap<u32, String>| {
        let cast = pat::param_casts(sql, "");
        let declared: BTreeMap<u32, String> = mine
            .iter()
            .map(|(n, t)| match other.get(n) {
                Some(o)
                    if t == "text"
                        && o != "text"
                        && !o.ends_with("[]")
                        && !cast.contains_key(n) =>
                {
                    (*n, o.clone())
                }
                _ => (*n, t.clone()),
            })
            .collect();
        if declared == mine {
            return mine;
        }
        side_types(db, sql, ph, &declared).unwrap_or(mine)
    };
    let types_b = prefer(db, b, &ph_b, types_b, &types_a);
    let types_a = prefer(db, a, &ph_a, types_a, &types_b);
    let mut pgtypes: HashMap<u32, (VType, bool)> = HashMap::new();
    for &n in &pnums {
        let va = types_a.get(&n).and_then(|t| vtype_of(t));
        let vb = types_b.get(&n).and_then(|t| vtype_of(t));
        match (va, vb) {
            (Some(x), Some(y)) if x == y => {
                pgtypes.insert(n, x);
            }
            (Some(x), None) if !types_b.contains_key(&n) => {
                pgtypes.insert(n, x);
            }
            (None, Some(y)) if !types_a.contains_key(&n) => {
                pgtypes.insert(n, y);
            }
            _ => {}
        }
    }
    timing.ddl_ms = started.elapsed().as_secs_f64() * 1000.0;

    let mut full_rng = StdRng::seed_from_u64(cfg.seed);
    let mut small_rng = StdRng::seed_from_u64(cfg.seed ^ SMALL_STREAM);
    let small_trials = cfg.trials / 4;
    let mut last_err: Option<String> = None;
    let mut ok_trials = 0usize;
    let mut typed: HashSet<u32> = HashSet::new();
    if let Err(e) = db.exec("SAVEPOINT trial") {
        return Verdict::Error(e);
    }

    for i in 0..cfg.total_trials() {
        let t_trial = Instant::now();
        // By the trial's number, not the generator, so every value drawn is the one it would be.
        let clock = clocked.then(|| clocks[i % clocks.len()].as_str());
        if let Some(c) = clock {
            if let Err(e) = db.exec(&format!("SET LOCAL sqleq.clock = '{c}'")) {
                return Verdict::Error(e);
            }
        }
        let small = i % 5 == 4 && i / 5 < small_trials;
        let rng: &mut StdRng = if small {
            &mut small_rng
        } else {
            &mut full_rng
        };
        let mut rowdata: RowData = RowData::new();
        for t in &finals {
            let table = &schema[t];
            let size = if small {
                small_size(rng, cfg.nrows)
            } else {
                cfg.nrows
            };
            let mut rows: Vec<Vec<Val>> = Vec::with_capacity(size);
            for _ in 0..size {
                let row: Vec<Val> = table.cols.iter().map(|c| randval_col(c, rng)).collect();
                if table.admits(&rows, &row) {
                    rows.push(row);
                }
            }
            rowdata.insert(t.clone(), rows);
        }

        let mut binds: HashMap<u32, Val> = HashMap::new();
        let mut cutting: HashSet<u32> = HashSet::new();
        for &n in &pnums {
            // The type Postgres inferred decides the placeholder's shape, and the heuristics' column
            // link and cast only where their type fits it: a link to a same-named column of another
            // table, or of another type, is a wrong link.
            let pg = pgtypes.get(&n).copied();
            let fits = |vt: VType| pg.is_none_or(|(p, _)| compatible(p, vt));
            let linked = pcol.get(&n).and_then(|c| colloc.get(c));
            let loc = linked.filter(|(_, _, vt, _)| fits(*vt));
            let is_array = match pg {
                Some((_, arr)) => arr,
                None => parray.contains(&n),
            };
            if loc.is_none() != linked.is_none() || is_array != parray.contains(&n) {
                typed.insert(n);
            }
            let present: Vec<Val> = match loc {
                Some((t, idx, _, col_array)) => rowdata[t]
                    .iter()
                    .filter(|r| r[*idx] != Val::Null)
                    .flat_map(|r| match (&r[*idx], col_array) {
                        (Val::List(elems), true) => elems
                            .iter()
                            .filter(|e| **e != Val::Null)
                            .cloned()
                            .collect::<Vec<_>>(),
                        (v, _) => vec![v.clone()],
                    })
                    .collect(),
                None => Vec::new(),
            };
            let cast = pcast
                .get(&n)
                .map(|t| {
                    if is_array {
                        array_element_type(t)
                    } else {
                        t.clone()
                    }
                })
                .and_then(|t| cast_target(&t))
                .filter(|ct| loc.is_none() || *ct != CastTarget::V(VType::Varchar))
                .filter(|ct| match ct {
                    CastTarget::V(v) => fits(*v),
                    _ => true,
                });
            let col_survives_cast = match (cast, loc) {
                (Some(CastTarget::V(v)), Some((_, _, vt, _))) => v == *vt,
                (Some(_), _) => false,
                (None, _) => true,
            };
            // The heuristics' need types a column by its bare name, which may be a same-named
            // column of another table, so it too decides only where it fits.
            let need = pneed.get(&n).copied().filter(|need| match need {
                typing::Need::Type(CastTarget::V(v)) => fits(*v),
                typing::Need::NumericString => fits(VType::Integer) || fits(VType::Varchar),
                _ => true,
            });
            if need != pneed.get(&n).copied() {
                typed.insert(n);
            }
            let mut pick = |rng: &mut StdRng| -> Val {
                if need == Some(typing::Need::NumericString) {
                    return randval_need(typing::Need::NumericString, rng);
                }
                if col_survives_cast && !present.is_empty() && rng.random_bool(0.75) {
                    present.choose(rng).unwrap().clone()
                } else if let Some(ct) = cast {
                    randval_cast(ct, rng)
                } else if let Some((_, _, vt, _)) = loc {
                    randval(*vt, false, rng)
                } else if let Some(need) = need {
                    randval_need(need, rng)
                } else if let Some((vt, _)) = pg {
                    // Where `test_pair` falls back to an integer drawn from nothing, the type
                    // Postgres inferred for the placeholder decides the domain.
                    typed.insert(n);
                    randval(vt, false, rng)
                } else {
                    randval(VType::Integer, false, rng)
                }
            };
            let v = if let Some(v) = neutral.get(&n) {
                if small && rng.random_bool(0.5) {
                    cutting.insert(n);
                    let count = match counted.get(&n).and_then(|k| k.first()) {
                        Some(Count::Offset) => rng.random_range(1..=cfg.nrows.max(1)),
                        _ => rng.random_range(0..=cfg.nrows),
                    };
                    Val::Int(count as i64)
                } else {
                    v.clone()
                }
            } else if is_array {
                let k = rng.random_range(1..=3);
                let mut elems = Vec::with_capacity(k);
                for _ in 0..k {
                    elems.push(pick(rng));
                }
                Val::List(elems)
            } else {
                pick(rng)
            };
            binds.insert(n, v);
        }

        // Load the instance: one statement per table when every row is accepted, else row by row,
        // twice, so a child row whose parent comes later in name order still finds it.
        let mut kept: RowData = RowData::new();
        let all: Vec<String> = targets
            .iter()
            .filter_map(|(k, t)| insert_sql(t, &rowdata[k].iter().collect::<Vec<_>>()))
            .collect();
        timing.rows_tried += rowdata.values().map(Vec::len).sum::<usize>();
        let batched = all.is_empty()
            || db
                .exec(&format!("SAVEPOINT ins; {}; RELEASE SAVEPOINT ins", all.join("; ")))
                .is_ok();
        if batched {
            kept = rowdata.clone();
        } else {
            if let Err(e) = db.exec("ROLLBACK TO SAVEPOINT ins") {
                return Verdict::Error(e);
            }
            let mut pending: Vec<(String, usize)> = targets
                .iter()
                .flat_map(|(k, _)| (0..rowdata[k].len()).map(move |j| (k.clone(), j)))
                .collect();
            for _pass in 0..2 {
                let mut failed = Vec::new();
                for (k, j) in pending {
                    let t = &targets.iter().find(|(kk, _)| *kk == k).unwrap().1;
                    let Some(sql) = insert_sql(t, &[&rowdata[&k][j]]) else {
                        continue;
                    };
                    match db.exec(&format!("SAVEPOINT r; {sql}; RELEASE SAVEPOINT r")) {
                        Ok(()) => kept.entry(k.clone()).or_default().push(rowdata[&k][j].clone()),
                        Err(_) => {
                            if let Err(e) = db.exec("ROLLBACK TO SAVEPOINT r") {
                                return Verdict::Error(e);
                            }
                            failed.push((k, j));
                        }
                    }
                }
                pending = failed;
            }
        }
        for t in &finals {
            kept.entry(t.clone()).or_default();
        }
        timing.rows_kept += kept.values().map(Vec::len).sum::<usize>();

        let sub_a = substitute(a, &ph_a, &binds, &types_a);
        let sub_b = substitute(b, &ph_b, &binds, &types_b);
        let side_a = run_side(db, &sub_a, is_query_a, ret_a, &reset, &targets);
        let side_b = match &side_a {
            Ok(_) => run_side(db, &sub_b, is_query_b, ret_b, &reset, &targets),
            Err(_) => Err(String::new()),
        };
        let (ra, rb) = match (side_a, side_b) {
            (Ok(ra), Ok(rb)) => (ra, rb),
            (Err(e), _) | (_, Err(e)) => {
                if let Err(e2) = finish(db) {
                    return Verdict::Error(e2);
                }
                if e.contains("statement timeout") {
                    return Verdict::Error(e);
                }
                if !e.is_empty() {
                    last_err = Some(e);
                }
                timing.trial_ms.push(t_trial.elapsed().as_secs_f64() * 1000.0);
                continue;
            }
        };
        ok_trials += 1;
        if misaligned.is_some() {
            let _ = finish(db);
            break;
        }
        let cuts_here =
            |c: &limits::Cut| !c.total && c.params.iter().any(|(n, _)| cutting.contains(n));
        let trial_nondet = nondet || cuts.iter().any(cuts_here);
        // A count bound to cut, in a cut whose tied rows a level above can tell apart: nothing to
        // compare on this trial.
        let trial_blind = cuts.iter().any(|c| !c.counted && cuts_here(c));
        let size = |bags: &[Bag]| bags.iter().map(|b| b.rows.len()).sum::<usize>();
        let differs = ra.len() != rb.len()
            || ra.iter().zip(&rb).any(|(x, y)| x.label != y.label || x.keys() != y.keys());
        let mut verdict = None;
        if differs && !trial_blind && !(trial_nondet && size(&ra) == size(&rb)) {
            // Text differs. A bag whose sizes differ is a difference whatever the values; one of
            // equal size may still hold the same values under `=`.
            let mut real = false;
            let mut unknown = false;
            for (x, y) in ra.iter().zip(&rb) {
                if x.keys() == y.keys() {
                    continue;
                }
                if x.rows.len() != y.rows.len() || x.label != y.label {
                    real = true;
                    break;
                }
                let tx = x.types.clone().or_else(|| result_types(db, &sub_a));
                let ty = y.types.clone().or_else(|| result_types(db, &sub_b));
                match (tx, ty) {
                    (Some(tx), Some(ty)) => match same_under_eq(db, x, &tx, y, &ty) {
                        Ok(true) => {}
                        Ok(false) => {
                            real = true;
                            break;
                        }
                        Err(e) => {
                            unknown = true;
                            last_err = Some(format!("uncomparable: {e}"));
                        }
                    },
                    _ => unknown = true,
                }
            }
            if ra.len() != rb.len() {
                real = true;
            }
            if real {
                verdict = Some(Verdict::NotEquivalent(describe(&binds, &kept, clock)));
            } else if unknown {
                timing.uncomparable += 1;
            } else {
                timing.eq_agreed += 1;
            }
        }
        if let Err(e) = finish(db) {
            return Verdict::Error(e);
        }
        timing.trial_ms.push(t_trial.elapsed().as_secs_f64() * 1000.0);
        if let Some(v) = verdict {
            timing.typed_params = typed.len();
            return v;
        }
    }
    timing.typed_params = typed.len();

    if let Some(detail) = misaligned {
        return match (ok_trials, last_err) {
            (0, Some(e)) => Verdict::Error(e),
            _ => Verdict::ParamMisaligned(detail),
        };
    }
    match (ok_trials, last_err) {
        (0, Some(e)) => Verdict::Error(e),
        (_, Some(e)) => Verdict::NoCounterexamplePartial {
            ok: ok_trials,
            last_err: e,
        },
        (_, None) => Verdict::NoCounterexample,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untyped_literals_quote_every_value() {
        assert_eq!(pg_lit(&Val::Int(1)), "'1'");
        assert_eq!(pg_lit(&Val::Str("o'k".into())), "'o''k'");
        assert_eq!(pg_lit(&Val::Null), "NULL");
        assert_eq!(
            pg_lit(&Val::List(vec![Val::Str("a\"b".into()), Val::Int(2)])),
            r#"'{"a\"b","2"}'"#
        );
    }

    #[test]
    fn an_icu_collation_is_recognised() {
        for s in [
            "CREATE COLLATION ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false)",
            "CREATE COLLATION c2 (PROVIDER = 'icu', LOCALE = 'de')",
            r#"SELECT a FROM t ORDER BY a COLLATE "und-x-icu""#,
            "SELECT icu_unicode_version()",
        ] {
            assert!(ICU.is_match(s), "{s}");
        }
        for s in [
            "CREATE COLLATION c3 (provider = libc, locale = 'C')",
            "SELECT unicode FROM t",
            r#"SELECT a FROM t ORDER BY a COLLATE "C""#,
        ] {
            assert!(!ICU.is_match(s), "{s}");
        }
    }

    #[test]
    fn a_default_calling_a_missing_function_is_dropped() {
        let ddl = "CREATE TABLE t (id uuid NOT NULL DEFAULT ext.gen_id(), n int DEFAULT 0);";
        assert_eq!(
            drop_defaults(ddl, "ext.gen_id").unwrap(),
            "CREATE TABLE t (id uuid NOT NULL, n int DEFAULT 0);"
        );
        assert!(drop_defaults(ddl, "other").is_none());
        let cast = "CREATE TABLE t (id text NOT NULL DEFAULT (gen_id())::text, b uuid DEFAULT gen_id()::uuid);";
        assert_eq!(
            drop_defaults(cast, "gen_id").unwrap(),
            "CREATE TABLE t (id text NOT NULL, b uuid);"
        );
    }

    #[test]
    fn a_table_named_by_one_schema_elsewhere_is_created_there() {
        let ddl = "CREATE TABLE t (id int);\nCREATE INDEX i ON app.t (id);";
        let placed = place_tables(ddl, ddl);
        assert!(placed.starts_with("CREATE TABLE \"app\".t (id int);"), "{placed}");
        // Two schemas, or none: left as written.
        let two = "CREATE TABLE t (id int); CREATE INDEX i ON a.t (id); CREATE INDEX j ON b.t (id);";
        assert_eq!(place_tables(two, two), two);
        let none = "CREATE TABLE t (id int); CREATE INDEX i ON public.t (id);";
        assert_eq!(place_tables(none, none), none);
    }
}
