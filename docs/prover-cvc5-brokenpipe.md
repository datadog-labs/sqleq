# DRAFT — not filed: prover crashes on large SMT formulas (`unify.rs:269` BrokenPipe)

**Status: draft, never filed.** The QED prover is a third-party repository. This note is written up
so the finding is not lost; it has not been filed with that project, and nothing here was submitted
or pushed anywhere. Publishing it in this repository is disclosure, not a bug report: it has had no
upstream review, and it may be wrong about that project's intent.

**Component:** `src/pipeline/unify.rs`, `fn smt`
**Affects:** qed-prover 0.1.0 as built by the prover repository's own flake (z3 4.12.2, cvc5 1.0.8)
**Impact:** any query pair whose SMT formula exceeds the pipe buffer can abort the whole run, discarding
a verdict the other solver was in the middle of producing correctly.

## What happens

```
thread '<unnamed>' panicked at src/pipeline/unify.rs:269:47:
called `Result::unwrap()` on an `Err` value: Os { code: 32, kind: BrokenPipe, message: "Broken pipe" }
thread 'main' panicked at src/pipeline/unify.rs:287:6:
called `Result::unwrap()` on an `Err` value: Any { .. }
```

The result file records `{"provable": false, "panicked": true}` and the run reports `Provable: 0 / 1`.

## Why

`smt()` races z3 and cvc5 on the same formula, one scoped thread per solver:

```rust
s.spawn(move |_| {
    cvc5_in.write_all("(set-logic ALL)".as_bytes()).unwrap();
    cvc5_in.write_all(smt.as_bytes()).unwrap();          // <-- line 269
    ...
});
let reason = p.park_timeout(Ctx::timeout());
z3_cmd.kill().unwrap();
...
(res.load(), matches!(reason, UnparkReason::Timeout))
})
.unwrap()                                                 // <-- line 287
```

Three properties combine:

1. **The write is unconditional and unwrapped.** If the child's stdin closes for *any* reason —
   the child exited on a command it rejected, the child was killed on the parked timeout, the child
   died — `write_all` returns `EPIPE` and the thread panics.
2. **A small formula hides it.** A pipe buffers 64 KiB. Below that, `write_all` completes into the
   buffer before the child can react, so the writer never observes the peer's state. The bug is
   therefore invisible on small inputs and reproducible on large ones.
3. **One racer's panic kills both.** `crossbeam::thread::scope(...).unwrap()` at `unify.rs:287` propagates
   the scoped thread's panic onto the main thread, so the run aborts even when the *other* solver has
   already answered, or would have.

The z3 writer at `unify.rs:256` has the identical `unwrap()` and the same exposure.

## Reproduction

Any pair whose formula exceeds 64 KiB. On one such pair:

- Deterministic: every run panics, always at `unify.rs:269`, always ~230s in.
- Not a timeout artifact: `QED_SMT_TIMEOUT=60000` (up from the 10s default) panics at the same place.
- Formula size measured by interposing a recording `cvc5` on `PATH`: **every SMT call ~160 KiB**, each
  one far past the 64 KiB buffer.

Confirming the mechanism end to end: replace `cvc5` on `PATH` with a stub that drains stdin and answers
nothing (indistinguishable, to the prover, from a cvc5 that is simply slower than z3). The run then
completes and reports **`Provable: 1 / 1`**. Nothing about the input changed; only the writer's ability
to observe a closed peer did.

This is also why the bug is silent about its own cost: the verdict was available the whole time.

## Suggested fix

Do not `unwrap()` the write to a racing solver. A broken pipe means "this solver is out of the race",
which is already a state the design handles — the two answers are combined by OR'ing `unsat`, so a
solver that contributes nothing costs speed and never correctness:

```rust
s.spawn(move |_| {
    let wrote = cvc5_in
        .write_all("(set-logic ALL)".as_bytes())
        .and_then(|()| cvc5_in.write_all(smt.as_bytes()));
    drop(cvc5_in);
    if wrote.is_err() {
        // cvc5 is out of the race; z3's answer stands.
        if last.fetch_or(true) { u2.unpark(); }
        return;
    }
    ...
});
```

and the same at `unify.rs:256` for z3. The `.unwrap()` at `unify.rs:287` is then unreachable for this
cause; making it a recorded error rather than a panic would also stop a single racer from erasing a
completed verdict.

## Not the same as two other prover failures we have seen

| signature | how it shows |
|---|---|
| z3 null-deref | `SIGSEGV`, `rc=-11` |
| serde recursion limit | `ParseErr("recursion limit exceeded")`, recorded as `panicked` |
| **this one** | `BrokenPipe` at `unify.rs:269` → re-panic at `:287` |
