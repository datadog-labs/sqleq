// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Subprocesses with a hard wall-clock timeout.
//!
//! Every backend runs in a process group of its own, and a timeout kills the whole group, so the
//! prover's z3/cvc5 children or Lean's `lean` under `lake` are not orphaned. The same goes for
//! Ctrl-C: the groups are not in the terminal's foreground group, so they never see the signal
//! themselves; [`install_interrupt_handler`] kills every live one before the harness exits.

use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct Run {
    /// The exit code, or minus the signal that ended the process.
    pub rc: i32,
    pub out: String,
    pub err: String,
    pub timed_out: bool,
    /// Wall time in seconds.
    pub wall: f64,
}

/// Live process groups, and whether an interrupt has already killed them. One lock, so a backend
/// spawned while the interrupt is being handled is killed rather than left behind.
struct Live {
    groups: Vec<i32>,
    interrupted: bool,
}

static LIVE: Mutex<Live> = Mutex::new(Live { groups: Vec::new(), interrupted: false });

fn kill_group(pgid: i32) {
    // SAFETY: plain syscall; a group that is already gone yields ESRCH, which is ignored.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

fn register(pgid: i32) {
    let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    if live.interrupted {
        kill_group(pgid);
    } else {
        live.groups.push(pgid);
    }
}

fn unregister(pgid: i32) {
    let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    live.groups.retain(|g| *g != pgid);
}

/// Kill every live backend. Called on an interrupt; every later spawn is killed on arrival.
pub fn kill_all() {
    let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    live.interrupted = true;
    for g in live.groups.drain(..) {
        kill_group(g);
    }
}

static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(sig: libc::c_int) {
    let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let b = sig as u8;
        // SAFETY: write(2) is async-signal-safe; the buffer is a live local byte.
        unsafe {
            libc::write(fd, (&b as *const u8).cast(), 1);
        }
    }
}

/// On SIGINT or SIGTERM: kill every live backend's process group, say so, and exit `128 + signal`
/// (130 for Ctrl-C). The handler only writes the signal to a pipe; a thread does the rest.
pub fn install_interrupt_handler() {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors pipe(2) writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    SIGNAL_PIPE.store(fds[1], Ordering::Relaxed);
    let read_fd = fds[0];
    std::thread::spawn(move || {
        let mut b = 0u8;
        // SAFETY: reads one byte into a live local.
        let n = unsafe { libc::read(read_fd, (&mut b as *mut u8).cast(), 1) };
        if n == 1 {
            kill_all();
            eprintln!("\ninterrupted");
            std::process::exit(128 + i32::from(b));
        }
    });
    for sig in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: installs a handler that only calls write(2).
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as *const () as usize;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

enum Event {
    Out(Vec<u8>),
    Err(Vec<u8>),
    Exit(ExitStatus),
}

/// Run `argv` to completion, or until `timeout` seconds have passed, when its whole process group
/// is killed. `env` is laid over the harness's own environment; `cwd` defaults to its own.
pub fn run(argv: &[String], cwd: Option<&Path>, env: &[(String, String)], timeout: Option<f64>) -> Run {
    let t0 = Instant::now();
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Run {
                rc: 127,
                err: format!("cannot run {}: {e}", argv[0]),
                wall: t0.elapsed().as_secs_f64(),
                ..Run::default()
            }
        }
    };
    let pgid = child.id() as i32;
    register(pgid);

    let (tx, rx) = mpsc::channel();
    let mut so = child.stdout.take().expect("piped stdout");
    let mut se = child.stderr.take().expect("piped stderr");
    let txo = tx.clone();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        let _ = txo.send(Event::Out(b));
    });
    let txe = tx.clone();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        let _ = txe.send(Event::Err(b));
    });
    std::thread::spawn(move || {
        if let Ok(st) = child.wait() {
            let _ = tx.send(Event::Exit(st));
        }
    });

    // Done when the process has exited *and* both pipes are closed: a child it left behind holding
    // them open is still running, and is killed with the group at the deadline.
    let deadline = timeout.filter(|t| t.is_finite()).map(|t| t0 + Duration::from_secs_f64(t.max(0.0)));
    let (mut out, mut err, mut status) = (None, None, None);
    let mut timed_out = false;
    let mut collect = |ev: Event| match ev {
        Event::Out(b) => out = Some(b),
        Event::Err(b) => err = Some(b),
        Event::Exit(s) => status = Some(s),
    };
    let mut pending = 3;
    while pending > 0 {
        let ev = match deadline {
            None => rx.recv().ok(),
            Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                Ok(ev) => Some(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    timed_out = true;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
            },
        };
        match ev {
            Some(ev) => {
                collect(ev);
                pending -= 1;
            }
            None => break,
        }
    }
    if timed_out {
        kill_group(pgid);
        // The group is dead, so its pipes close; give it a moment to be reaped.
        let grace = Instant::now() + Duration::from_secs(10);
        while pending > 0 {
            match rx.recv_timeout(grace.saturating_duration_since(Instant::now())) {
                Ok(ev) => {
                    collect(ev);
                    pending -= 1;
                }
                Err(_) => break,
            }
        }
    }
    unregister(pgid);
    let rc = status.map_or(0, |s| s.code().unwrap_or_else(|| -s.signal().unwrap_or(0)));
    let text = |b: Option<Vec<u8>>| b.map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    Run { rc, out: text(out), err: text(err), timed_out, wall: t0.elapsed().as_secs_f64() }
}

/// [`run`] in `cwd` with a timeout, the shape every per-case stage uses.
pub fn run_cmd(argv: &[String], cwd: &Path, timeout: f64, env: &[(String, String)]) -> Run {
    run(argv, Some(cwd), env, Some(timeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Vec<String> {
        vec!["/bin/sh".into(), "-c".into(), script.into()]
    }

    #[test]
    fn output_and_exit_code_are_captured() {
        let r = run(&sh("echo out; echo err >&2; exit 3"), None, &[], Some(10.0));
        assert_eq!((r.rc, r.out.as_str(), r.err.as_str(), r.timed_out), (3, "out\n", "err\n", false));
    }

    #[test]
    fn a_timeout_kills_the_whole_group() {
        let dir = crate::util::TempDir::new("sqleq-proc-test-").unwrap();
        let pidfile = dir.path().join("pid");
        // The grandchild writes its pid and sleeps; the shell waits on it.
        let script = format!("sh -c 'echo $$ > {}; exec sleep 30' & wait", pidfile.display());
        let r = run(&sh(&script), None, &[], Some(0.5));
        assert!(r.timed_out);
        assert!(r.wall < 5.0, "{}", r.wall);
        let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!alive(pid), "grandchild {pid} survived the timeout");
    }

    /// Running, as opposed to gone or a zombie waiting for whoever adopted it to reap it.
    pub(crate) fn alive(pid: i32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            // The state is the field after the parenthesised command name.
            Ok(stat) => stat.rsplit(')').next().and_then(|s| s.split_whitespace().next()) != Some("Z"),
            Err(_) => false,
        }
    }

    #[test]
    fn env_is_laid_over_ours() {
        let r = run(&sh("echo $SQLEQ_PROC_TEST"), None, &[("SQLEQ_PROC_TEST".into(), "x".into())], None);
        assert_eq!(r.out, "x\n");
    }

    #[test]
    fn a_missing_binary_is_an_error_not_a_panic() {
        let r = run(&["/nonexistent/binary".to_string()], None, &[], Some(1.0));
        assert_eq!(r.rc, 127);
        assert!(r.err.contains("cannot run"));
    }
}
