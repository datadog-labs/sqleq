// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

/**
 * Runs SQLSolver over plans built from our IR instead of over SQL text.
 *
 * <p>Reads the job file produced by {@code sqleq-frontend --sqlsolver --ir}, translates each
 * row with {@link IrToRel}, and calls
 * {@code Verification.verify(RelNode, RelNode, Schema)}.
 *
 * <p><b>Driver.java is deliberately untouched.</b> The canonical text run is the control that every
 * existing work dir's verdicts pair against, and the cheapest way to prove this change cannot move a
 * canonical number is to not share code with it. The worker/halt discipline below is therefore a
 * copy rather than a call: {@code Verification.verify} can hang in a way interrupts do not reach, so
 * a worker that will not stop takes the process down with {@code halt(3)} and the harness resumes on
 * a fresh JVM -- after the row has been written, so resume always makes progress.
 *
 * <p><b>Tier 0 is replicated, on purpose.</b> The plan entry skips {@code isLiteralEq}, and the cost
 * of that was measured: almost every EQ verdict the plan entry loses is a tier-0 row. Their
 * tier 0 compares two plans assembled from text; ours compares the two IR trees, which is the same
 * question asked one layer earlier -- and the same one QED asks itself at
 * {@code pipeline.rs:55}. It is recorded in {@code literal} exactly as {@code Driver} records it,
 * because a syntactic coincidence must never be counted as a proof.
 */
import java.io.BufferedReader;
import java.io.OutputStream;
import java.io.PrintStream;
import java.io.Writer;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardOpenOption;
import java.util.concurrent.atomic.AtomicReference;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

import sqlsolver.calcite.jdbc.CalciteSchema;
import sqlsolver.calcite.rel.RelNode;

import sqlsolver.api.entry.Verification;
import sqlsolver.sql.rel.RelSupport;
import sqlsolver.sql.schema.Schema;

public final class IrDriver {

  private static long capMs = 60_000;
  private static long graceMs = 5_000;
  private static final ObjectMapper MAPPER = new ObjectMapper();

  public static void main(String[] args) throws Exception {
    if (args.length < 2) {
      System.err.println("usage: IrDriver <ir.jobs.jsonl> <results.jsonl> "
          + "[--timeout-ms=N] [--grace-ms=N] [--dry-run]");
      System.exit(2);
    }
    final Path jobs = Paths.get(args[0]);
    final Path out = Paths.get(args[1]);
    boolean dry = false;
    for (int k = 2; k < args.length; k++) {
      final String a = args[k];
      if (a.startsWith("--timeout-ms=")) capMs = Long.parseLong(a.substring(13));
      else if (a.startsWith("--grace-ms=")) graceMs = Long.parseLong(a.substring(11));
      // Translate every row and report what happened, without calling their prover. This is the
      // Stage 0 expressibility question answered by the real translator rather than by a model of it.
      else if (a.equals("--dry-run")) dry = true;
      else {
        System.err.println("unknown argument: " + a);
        System.exit(2);
      }
    }
    System.setOut(new PrintStream(OutputStream.nullOutputStream()));

    try (BufferedReader in = Files.newBufferedReader(jobs);
        Writer w = Files.newBufferedWriter(out, StandardOpenOption.CREATE,
            StandardOpenOption.APPEND)) {
      String line;
      while ((line = in.readLine()) != null) {
        if (line.trim().isEmpty()) continue;
        final JsonNode job = MAPPER.readTree(line);
        final String name = job.path("name").asText("?");
        final Result r = run(job, dry);
        w.write(r.json(name));
        w.write("\n");
        w.flush();
        System.err.println(name + " " + r.verdict + " " + r.ms + "ms"
            + (r.refused != null ? " refused=" + r.refused : ""));
        if (r.hung) {
          System.err.println("driver: " + name + " did not stop when interrupted; halting so the "
              + "harness restarts on a fresh process");
          Runtime.getRuntime().halt(3);
        }
      }
    }
  }

  private static final class Result {
    String verdict;
    long ms;
    boolean killed, hung;
    String refused, error;
    Boolean literal;

    String json(String name) {
      final StringBuilder sb = new StringBuilder(128);
      sb.append("{\"name\":").append(quote(name));
      sb.append(",\"verdict\":").append(quote(verdict));
      sb.append(",\"ms\":").append(ms);
      sb.append(",\"killed\":").append(killed);
      if (literal != null) sb.append(",\"literal\":").append(literal.booleanValue());
      if (refused != null) sb.append(",\"refused\":").append(quote(refused));
      if (error != null) sb.append(",\"error\":").append(quote(error));
      return sb.append('}').toString();
    }
  }

  private static Result run(JsonNode job, boolean dry) {
    final Result r = new Result();
    final long t0 = System.currentTimeMillis();

    final JsonNode ir = job.get("ir");
    if (ir == null || ir.isNull()) {
      // The row exists in the job file even when the frontend declined it, so both runs cover the
      // same names and a missing row can never be mistaken for a lost one.
      r.verdict = "NOIR";
      r.refused = job.path("refusal").asText("frontend");
      r.ms = System.currentTimeMillis() - t0;
      return r;
    }

    // Tier 0, before anything else: two identical trees are equal whatever the prover thinks, and
    // asking here costs no Calcite at all.
    final JsonNode qs = ir.get("queries");
    if (qs != null && qs.size() == 2 && qs.get(0).equals(qs.get(1))) {
      r.verdict = "EQ";
      r.literal = Boolean.TRUE;
      r.ms = System.currentTimeMillis() - t0;
      return r;
    }

    final AtomicReference<String> verdict = new AtomicReference<>(null);
    final AtomicReference<String> refused = new AtomicReference<>(null);
    final AtomicReference<String> error = new AtomicReference<>(null);
    final String ddl = job.path("schema").asText("");

    final Thread worker = new Thread(() -> {
      try {
        // Inside the bounded worker: parsing the DDL and translating are both real work, and the
        // point of the cap is that no row can stall the harness for any reason.
        final CalciteSchema calcite = RelSupport.getCalciteSchema(ddl);
        final Schema schema = RelSupport.getSchema(ddl);
        final RelNode[] plans = IrToRel.build(ir, calcite);
        if (dry) {
          verdict.set("TRANSLATED");
          return;
        }
        verdict.set(String.valueOf(Verification.verify(plans[0], plans[1], schema)));
      } catch (IrToRel.Refused t) {
        refused.set(t.getMessage());
      } catch (Throwable t) {
        error.set(t.getClass().getName() + (t.getMessage() == null ? "" : ": " + t.getMessage()));
      }
    }, "ir-verify");
    worker.setDaemon(true);
    worker.start();
    try {
      worker.join(capMs);
      if (worker.isAlive()) {
        r.killed = true;
        worker.interrupt();
        worker.join(graceMs);
        r.hung = worker.isAlive();
      }
    } catch (InterruptedException e) {
      r.killed = true;
      r.hung = worker.isAlive();
    }
    r.ms = System.currentTimeMillis() - t0;
    r.refused = refused.get();
    r.error = error.get();
    final String v = verdict.get();
    // NOTRANS is kept apart from UNKNOWN throughout: UNKNOWN is their prover declining to decide,
    // NOTRANS is us never handing it a plan. Conflating them is what made their parser look like
    // their prover in the first place, and it would do the same to ours.
    r.verdict = v != null ? v
        : r.refused != null ? "NOTRANS"
        : r.error != null ? "ERROR"
        : r.hung ? "HANG" : "TIMEOUT";
    // Only meaningful against their prover, and only on EQ -- an EQ reached here is not tier 0,
    // because tier 0 already returned above.
    if ("EQ".equals(r.verdict)) r.literal = Boolean.FALSE;
    return r;
  }

  private static String quote(String v) {
    final StringBuilder sb = new StringBuilder(v.length() + 8).append('"');
    for (int i = 0; i < v.length(); i++) {
      final char c = v.charAt(i);
      switch (c) {
        case '"': sb.append("\\\""); break;
        case '\\': sb.append("\\\\"); break;
        case '\n': sb.append("\\n"); break;
        case '\r': sb.append("\\r"); break;
        case '\t': sb.append("\\t"); break;
        default:
          if (c < 0x20) sb.append(String.format("\\u%04x", (int) c));
          else sb.append(c);
      }
    }
    return sb.append('"').toString();
  }
}
