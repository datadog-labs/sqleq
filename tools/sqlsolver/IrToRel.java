// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

/**
 * Turns our lowered relational IR into a SQLSolver {@code RelNode}, so its prover can be reached
 * through {@code Verification.verify(RelNode, RelNode, Schema)} instead of through its MySQL parser.
 *
 * <p>This is the shared-frontend bridge. The sqlsolver axis loses most of its no-proofs inside
 * Calcite's parser, on queries our own resolver already handles; handing it a plan skips that
 * layer entirely.
 *
 * <p><b>Stage 2: the nodes are the shim's, not Calcite's.</b> {@code sqlsolver.calcite.*} mirrors
 * Calcite's package and class names so that SQLSolver's translator needed only its import
 * block changed. Here on our side the change is larger in kind though smaller in size: there is no
 * {@code RelBuilder} any more, so this file owns what the builder used to do -- row-type derivation
 * and literal construction. It builds each node directly and folds nothing.
 *
 * <p><b>Where that can differ from the pre-ectomy run, named in advance.</b> {@code RelBuilder}
 * rewrote some plans on the way in, even with {@code withSimplify(false)}. Three of its rewrites
 * were observed to fire:
 *
 * <ul>
 *   <li>a conjunct that is always false (an {@code IS NULL} on a NOT NULL column, say) collapsed the
 *       whole subtree to an empty {@code LogicalValues};
 *   <li>a {@code project} directly over a {@code project} was merged into one;
 *   <li>an {@code Aggregate} over a {@code Project} sometimes absorbed it, and an {@code Aggregate}
 *       with duplicate aggregate calls grew a {@code Project} above it.
 * </ul>
 *
 * <p>None of those is semantics-preserving in the only sense that matters here -- they change the
 * plan SQLSolver is asked about -- so this file reproduces none of them, and the A/B diff reports
 * how many verdicts turn out to depend on them. That is the approved plan's instruction ("the shim
 * will not; verdicts must not depend on that -- if one does, say which") rather than an oversight.
 *
 * <p><b>Why the two IRs line up.</b> The conventions are the same one, not two that happen to agree:
 * a column is a flat positional index into the concatenated input row, which is exactly
 * {@code RexInputRef}; {@code Group} is keys-then-aggregates, which is exactly Calcite's
 * {@code Aggregate}; {@code Join} is left-then-right, {@code Union}/{@code Except} take their row
 * type from the first arm. Each of those was read off {@code prover/src/pipeline/relation.rs}'s
 * {@code scope()} rather than assumed, because a mismatch here does not fail -- it silently builds a
 * different query and any verdict from it is worthless.
 *
 * <p><b>What it refuses, and why refusing is the right answer.</b> Every refusal below is a case
 * where SQLSolver would answer UNKNOWN anyway, or where we cannot express the row without changing
 * its meaning. Refusals are named and counted so the gap is a measurement rather than a mystery:
 *
 * <ul>
 *   <li>{@code correlated} -- a column index below the subquery's base is a reference to an
 *       enclosing row. Calcite spells that {@code RexFieldAccess} over a {@code RexCorrelVariable}
 *       with the id hung on the enclosing {@code Filter}, which their translator does read
 *       ({@code UExprConcreteTranslator:1277,1596}) but only one id per filter. A minority of rows
 *       need it; the rest do not, so the bridge delivers those and leaves this quantified.
 *   <li>{@code aggregate:*} -- their aggregate switch ends in {@code default -> return null}
 *       ({@code UExprConcreteTranslator:~760}), so an aggregate outside COUNT/SUM/AVG/MAX/MIN and
 *       the stat family yields no translation on their side either. Minting an aggregate function
 *       for our opaque {@code DISTINCT_ON#k} carriers would buy exactly nothing. Only a handful of
 *       rows need one, and nearly all of those are {@code DISTINCT_ON}.
 *   <li>{@code values-nonliteral} -- {@code LogicalValues} holds literals only.
 *   <li>{@code group-key-nonref} / {@code agg-arg-nonref} -- an {@code Aggregate} addresses its
 *       input by ordinal, so a computed group key or aggregate argument needs a {@code Project}
 *       underneath, which is the one rewrite {@code RelBuilder}'s registrar performed for us.
 *       A census over every job found many thousands of group keys and aggregate arguments and
 *       <em>zero</em> of either that is not a bare column, so the registrar never fired and this
 *       refusal is unreachable against today's IR. It is here so that a future IR that does need it
 *       is refused loudly rather than mis-built.
 * </ul>
 *
 * <p><b>Operators.</b> Only operators whose semantics are identical in both systems get a real
 * {@code SqlKind}; the table below reproduces the {@code (name, kind)} pair Calcite's
 * {@code SqlStdOperatorTable} gave each one, read off a live Calcite rather than remembered -- which
 * is how {@code CONCAT} turns out to be kind {@code OTHER} and not {@code OTHER_FUNCTION}.
 * Everything else -- the great majority of the distinct operators that occur, all Postgres builtins,
 * plus our own {@code QP}/{@code QCAST} carriers -- becomes an uninterpreted function keyed by
 * name, reaching their translator as {@code OTHER_FUNCTION} and becoming a {@code UFunc}
 * ({@code UExprConcreteTranslator:1502}). That is the mirror of our own opaque-carrier hatch, and it
 * is the safe default: an uninterpreted symbol can only cost proving power, while a wrong mapping to
 * a real operator would cost soundness. Both sides of a pair mint the same name, so cross-side
 * identity -- the thing a parameter carrier exists to preserve -- survives.
 *
 * <p>{@code COALESCE}, {@code NULLIF} and {@code IS DISTINCT FROM} are deliberately given their real
 * kinds even though the translator's switch does not handle them: under real Calcite they arrived
 * with those kinds too and fell through to {@code "Unsupported value kind"}. Re-routing them to
 * {@code OTHER_FUNCTION} would make them translate, which would be a capability gain manufactured by
 * the ectomy rather than measured.
 */
import java.math.BigDecimal;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

import com.fasterxml.jackson.databind.JsonNode;
import com.google.common.collect.ImmutableList;

import sqlsolver.calcite.jdbc.CalciteSchema;
import sqlsolver.calcite.plan.RelOptTable;
import sqlsolver.calcite.rel.RelCollations;
import sqlsolver.calcite.rel.RelFieldCollation;
import sqlsolver.calcite.rel.RelNode;
import sqlsolver.calcite.rel.core.AggregateCall;
import sqlsolver.calcite.rel.core.JoinRelType;
import sqlsolver.calcite.rel.logical.LogicalAggregate;
import sqlsolver.calcite.rel.logical.LogicalFilter;
import sqlsolver.calcite.rel.logical.LogicalIntersect;
import sqlsolver.calcite.rel.logical.LogicalJoin;
import sqlsolver.calcite.rel.logical.LogicalMinus;
import sqlsolver.calcite.rel.logical.LogicalProject;
import sqlsolver.calcite.rel.logical.LogicalSort;
import sqlsolver.calcite.rel.logical.LogicalTableScan;
import sqlsolver.calcite.rel.logical.LogicalUnion;
import sqlsolver.calcite.rel.logical.LogicalValues;
import sqlsolver.calcite.rel.type.RelDataType;
import sqlsolver.calcite.rel.type.RelDataTypeFactory;
import sqlsolver.calcite.rex.RexBuilder;
import sqlsolver.calcite.rex.RexInputRef;
import sqlsolver.calcite.rex.RexLiteral;
import sqlsolver.calcite.rex.RexCall;
import sqlsolver.calcite.rex.RexNode;
import sqlsolver.calcite.rex.RexSubQuery;
import sqlsolver.calcite.sql.SqlAggFunction;
import sqlsolver.calcite.sql.SqlKind;
import sqlsolver.calcite.sql.SqlOperator;
import sqlsolver.calcite.sql.type.SqlTypeName;
import sqlsolver.calcite.util.ImmutableBitSet;

final class IrToRel {

  /** A row we decline to translate, carrying the reason so the harness can count the families. */
  static final class Refused extends RuntimeException {
    Refused(String why) {
      super(why, null, false, false); // no stack trace: this is data, not a failure
    }
  }

  private final CalciteSchema schema;
  private final RexBuilder rex;
  private final List<String> tables;
  private final Map<String, SqlOperator> minted = new HashMap<>();

  private IrToRel(CalciteSchema schema, List<String> tables) {
    this.schema = schema;
    this.rex = new RexBuilder(new RelDataTypeFactory());
    this.tables = tables;
  }

  /** Builds both sides of a pair against one schema. */
  static RelNode[] build(JsonNode ir, CalciteSchema schema) {
    final List<String> tables = new ArrayList<>();
    // `path` not `get`: an `Input` with no `schemas` at all iterates as empty rather than throwing,
    // and then any `{"scan": i}` refuses as `scan-out-of-range` -- loud, and in the right place.
    for (JsonNode s : ir.path("schemas")) {
      // `name` is an additive field: plans lowered before it existed carry none, and
      // `sqleq_check.py` takes an archived or hand-written `Input` as a first-class case. The
      // fallback is the very spelling `sqlsolver::ddl_from_ir` emits for a nameless schema, so a
      // `{"scan": i}` still addresses the table that entry describes -- which is the invariant the
      // bridge rests on.
      // Without it `s.get("name")` is null and the row dies as a NullPointerException.
      tables.add(s.path("name").asText("t" + tables.size()));
    }
    final IrToRel t = new IrToRel(schema, tables);
    final JsonNode qs = ir.get("queries");
    return new RelNode[] {t.rel(qs.get(0), 0), t.rel(qs.get(1), 0)};
  }

  // ---------------------------------------------------------------- relations

  /**
   * @param base width of every enclosing row, i.e. the index at which this subtree's own columns
   *     start. Our lowering numbers a subquery's columns from the enclosing scope's width
   *     ({@code lower.rs}: {@code offset = Scope::outer_width(outer)}), so {@code base} is exactly
   *     the boundary between a correlated reference and a local one.
   */
  private RelNode rel(JsonNode r, int base) {
    if (r == null || !r.isObject()) throw new Refused("rel-not-object");

    if (r.has("scan")) {
      final int i = r.get("scan").asInt();
      if (i < 0 || i >= tables.size()) throw new Refused("scan-out-of-range");
      final RelOptTable table = schema.getTable(tables.get(i));
      if (table == null) throw new Refused("scan-unknown-table");
      return LogicalTableScan.create(table);
    }
    if (r.has("distinct")) {
      final JsonNode v = r.get("distinct");
      if (!v.isObject()) throw new Refused("distinct-not-a-relation");
      final RelNode src = rel(v, base);
      // DISTINCT is a group on every column with no aggregates, which is what RelBuilder.distinct
      // expanded to as well.
      return LogicalAggregate.create(
          src, ImmutableBitSet.range(0, src.getRowType().getFieldCount()), List.of());
    }
    if (r.has("filter")) {
      final JsonNode v = r.get("filter");
      final RelNode src = rel(v.get("source"), base);
      final RexNode cond = expr(v.get("condition"), base, src.getRowType(), 0);
      return LogicalFilter.create(src, cond);
    }
    if (r.has("project")) {
      final JsonNode v = r.get("project");
      final RelNode src = rel(v.get("source"), base);
      final List<RexNode> out = new ArrayList<>();
      for (JsonNode e : v.get("target")) out.add(expr(e, base, src.getRowType(), 0));
      return LogicalProject.create(src, out);
    }
    if (r.has("join")) {
      final JsonNode v = r.get("join");
      final RelNode l = rel(v.get("left"), base);
      final int lw = l.getRowType().getFieldCount();
      final RelNode rr = rel(v.get("right"), base + lw);
      final JoinRelType kind = joinKind(v.get("kind").asText());
      final RexNode cond = expr(v.get("condition"), base, l.getRowType(), rr.getRowType(), 0);
      return LogicalJoin.create(l, rr, cond, kind);
    }
    if (r.has("group")) {
      final JsonNode v = r.get("group");
      final RelNode src = rel(v.get("source"), base);
      final RelDataType in = src.getRowType();
      final List<Integer> keys = new ArrayList<>();
      for (JsonNode k : v.get("keys")) keys.add(ordinal(k, base, in, "group-key-nonref"));
      final ImmutableBitSet groupSet = ImmutableBitSet.of(keys);
      final List<AggregateCall> calls = new ArrayList<>();
      for (JsonNode f : v.get("function")) calls.add(agg(f, base, in, groupSet.cardinality()));
      return LogicalAggregate.create(src, groupSet, calls);
    }
    if (r.has("sort")) {
      final JsonNode v = r.get("sort");
      final RelNode src = rel(v.get("source"), base);
      final RelDataType in = src.getRowType();
      final List<RelFieldCollation> cols = new ArrayList<>();
      for (JsonNode c : v.get("collation")) cols.add(collation(c));
      final RexNode off = optExpr(v.get("offset"), base, in);
      final RexNode fetch = optExpr(v.get("limit"), base, in);
      return LogicalSort.create(src, RelCollations.of(cols), off, fetch);
    }
    if (r.has("values")) return values(r.get("values"));
    if (r.has("union")) return setOp(r.get("union"), base, "union");
    if (r.has("except")) return setOp(r.get("except"), base, "except");
    if (r.has("intersect")) return setOp(r.get("intersect"), base, "intersect");

    throw new Refused("rel:" + r.fieldNames().next());
  }

  /**
   * {@code union} is bag union and the others are set operations -- {@code lower.rs:617-633} emits a
   * bare {@code union} only for UNION ALL, wraps UNION in {@code distinct}, and refuses INTERSECT
   * ALL / EXCEPT ALL outright. So {@code all} is true only for union.
   */
  private RelNode setOp(JsonNode arms, int base, String kind) {
    if (arms.size() != 2) throw new Refused(kind + "-arity");
    final List<RelNode> inputs = List.of(rel(arms.get(0), base), rel(arms.get(1), base));
    switch (kind) {
      case "union": return LogicalUnion.create(inputs, true);
      case "except": return LogicalMinus.create(inputs, false);
      default: return LogicalIntersect.create(inputs, false);
    }
  }

  private RelNode values(JsonNode v) {
    final List<RelDataType> types = new ArrayList<>();
    final List<String> names = new ArrayList<>();
    for (JsonNode t : v.get("schema")) {
      names.add("c" + types.size());
      types.add(type(t.asText()));
    }
    final RelDataType rowType = RelDataType.struct(names, types);

    final ImmutableList.Builder<ImmutableList<RexLiteral>> rows = ImmutableList.builder();
    for (JsonNode row : v.get("content")) {
      final ImmutableList.Builder<RexLiteral> cells = ImmutableList.builder();
      for (JsonNode cell : row) {
        final RexNode e = expr(cell, 0, rowType, 0);
        // LogicalValues holds literals and nothing else. Our VALUES may carry a function call; that
        // would need a project over a one-row values, which no corpus row has yet asked for.
        if (!(e instanceof RexLiteral)) throw new Refused("values-nonliteral");
        cells.add((RexLiteral) e);
      }
      rows.add(cells.build());
    }
    return LogicalValues.create(rowType, rows.build());
  }

  private RelFieldCollation collation(JsonNode c) {
    // Three-element array, not an object: [columnIndex, type, "ASCENDING NULLS LAST"].
    if (!c.isArray() || c.size() < 3) throw new Refused("collation-shape");
    final int idx = c.get(0).asInt();
    final String how = c.get(2).asText();
    final RelFieldCollation.Direction dir =
        how.startsWith("DESC")
            ? RelFieldCollation.Direction.DESCENDING
            : RelFieldCollation.Direction.ASCENDING;
    final RelFieldCollation.NullDirection nulls =
        how.endsWith("NULLS FIRST")
            ? RelFieldCollation.NullDirection.FIRST
            : RelFieldCollation.NullDirection.LAST;
    return new RelFieldCollation(idx, dir, nulls);
  }

  private JoinRelType joinKind(String k) {
    switch (k) {
      case "INNER": return JoinRelType.INNER;
      case "LEFT": return JoinRelType.LEFT;
      case "RIGHT": return JoinRelType.RIGHT;
      case "FULL": return JoinRelType.FULL;
      // SEMI/ANTI are the two their translator does not accept (:1714-1757). We never emit them.
      default: throw new Refused("join-kind:" + k);
    }
  }

  /** Their aggregate switch, and nothing beyond it -- anything else translates to null on their side. */
  private AggregateCall agg(JsonNode f, int base, RelDataType in, int groupCount) {
    final String op = f.get("operator").asText();
    final SqlAggFunction fn = AGGREGATES.get(op);
    if (fn == null) throw new Refused("aggregate:" + op.split("#")[0]);
    final List<Integer> args = new ArrayList<>();
    if (f.has("operand")) {
      for (JsonNode e : f.get("operand")) args.add(ordinal(e, base, in, "agg-arg-nonref"));
    }
    return AggregateCall.create(fn, f.path("distinct").asBoolean(false), args, in, groupCount);
  }

  /** An {@code Aggregate} addresses its input by ordinal, so this must resolve to a bare column. */
  private int ordinal(JsonNode e, int base, RelDataType in, String why) {
    final RexNode node = expr(e, base, in, 0);
    if (!(node instanceof RexInputRef)) throw new Refused(why);
    return ((RexInputRef) node).getIndex();
  }

  // -------------------------------------------------------------- expressions

  private RexNode optExpr(JsonNode e, int base, RelDataType in) {
    return e == null || e.isNull() ? null : expr(e, base, in, 0);
  }

  private RexNode expr(JsonNode e, int base, RelDataType in, int unusedDepth) {
    return expr(e, base, in, null, unusedDepth);
  }

  /**
   * @param left the row an index addresses; {@code right}, when present, is concatenated after it,
   *     which is how a join condition sees the world in both systems.
   */
  private RexNode expr(JsonNode e, int base, RelDataType left, RelDataType right, int depth) {
    if (e == null || !e.isObject()) throw new Refused("expr-not-object");

    if (e.has("column")) {
      final int abs = e.get("column").asInt();
      if (abs < base) throw new Refused("correlated");
      final int i = abs - base;
      final int lw = left.getFieldCount();
      if (i < lw) return new RexInputRef(i, left.getFieldList().get(i).getType());
      if (right != null && i - lw < right.getFieldCount()) {
        return new RexInputRef(i, right.getFieldList().get(i - lw).getType());
      }
      throw new Refused("column-out-of-range");
    }

    final String op = e.path("operator").asText("");
    final RelDataType ty = type(e.path("type").asText(""));

    if (e.has("query")) return subQuery(e, op, base, left, right, depth);

    final JsonNode ops = e.path("operand");
    if (ops.isMissingNode() || ops.size() == 0) return literal(op, e.path("type").asText(""), ty);

    final List<RexNode> args = new ArrayList<>();
    for (JsonNode a : ops) args.add(expr(a, base, left, right, depth));

    if (op.equals("CAST")) {
      if (args.size() != 1) throw new Refused("cast-arity");
      return rex.makeAbstractCast(ty, args.get(0));
    }
    final SqlOperator std = standard(op, args.size());
    return new RexCall(std != null ? std : mint(op), args, ty);
  }

  private RexNode subQuery(JsonNode e, String op, int base, RelDataType left, RelDataType right,
      int depth) {
    // The subquery's own columns start after everything visible here; anything below that boundary
    // is a correlated reference, which `expr` refuses.
    final int inner = base + left.getFieldCount() + (right == null ? 0 : right.getFieldCount());
    final RelNode q = rel(e.get("query"), inner);
    switch (op) {
      case "EXISTS": return RexSubQuery.exists(q);
      case "$SCALAR_QUERY": return RexSubQuery.scalar(q);
      case "IN": {
        final List<RexNode> lhs = new ArrayList<>();
        for (JsonNode a : e.path("operand")) lhs.add(expr(a, base, left, right, depth));
        return RexSubQuery.in(q, lhs);
      }
      default: throw new Refused("subquery:" + op);
    }
  }

  /**
   * A 0-ary node is a literal and its value is the operator string. Only 5 type strings exist in the
   * whole corpus, and no VARCHAR literal collides with the QP/QCAST carrier namespace, so reading
   * the value off the operator is exact rather than merely usually right.
   */
  private RexNode literal(String value, String tyName, RelDataType ty) {
    if (value.equals("NULL")) return rex.makeNullLiteral(ty);
    try {
      switch (tyName) {
        case "INTEGER": return rex.makeExactLiteral(new BigDecimal(value), ty);
        case "REAL": return rex.makeApproxLiteral(new BigDecimal(value), ty);
        case "BOOLEAN": return rex.makeLiteral(Boolean.parseBoolean(value));
        // VARBINARY is our opaque catch-all. Its literals are opaque strings, and carrying them as
        // string constants keeps both sides identical, which is all the equality reasoning needs.
        default: return rex.makeLiteral(value);
      }
    } catch (NumberFormatException ex) {
      throw new Refused("literal:" + tyName);
    }
  }

  // ---------------------------------------------------------------- operators

  private static SqlOperator op(String name, SqlKind kind) {
    return new SqlOperator(name, kind);
  }

  private static SqlAggFunction aggFn(String name, SqlKind kind) {
    return new SqlAggFunction(name, kind);
  }

  private static final SqlOperator EQUALS = op("=", SqlKind.EQUALS);
  private static final SqlOperator NOT_EQUALS = op("<>", SqlKind.NOT_EQUALS);
  private static final SqlOperator LESS_THAN = op("<", SqlKind.LESS_THAN);
  private static final SqlOperator LESS_THAN_OR_EQUAL = op("<=", SqlKind.LESS_THAN_OR_EQUAL);
  private static final SqlOperator GREATER_THAN = op(">", SqlKind.GREATER_THAN);
  private static final SqlOperator GREATER_THAN_OR_EQUAL = op(">=", SqlKind.GREATER_THAN_OR_EQUAL);
  private static final SqlOperator AND = op("AND", SqlKind.AND);
  private static final SqlOperator OR = op("OR", SqlKind.OR);
  private static final SqlOperator NOT = op("NOT", SqlKind.NOT);
  private static final SqlOperator IS_NULL = op("IS NULL", SqlKind.IS_NULL);
  private static final SqlOperator IS_NOT_NULL = op("IS NOT NULL", SqlKind.IS_NOT_NULL);
  private static final SqlOperator IS_DISTINCT_FROM =
      op("IS DISTINCT FROM", SqlKind.IS_DISTINCT_FROM);
  private static final SqlOperator IS_NOT_TRUE = op("IS NOT TRUE", SqlKind.IS_NOT_TRUE);
  private static final SqlOperator CASE = op("CASE", SqlKind.CASE);
  private static final SqlOperator COALESCE = op("COALESCE", SqlKind.COALESCE);
  private static final SqlOperator NULLIF = op("NULLIF", SqlKind.NULLIF);
  private static final SqlOperator LIKE = op("LIKE", SqlKind.LIKE);
  // Calcite's CONCAT is kind OTHER, not OTHER_FUNCTION. Measured, not remembered.
  private static final SqlOperator CONCAT = op("||", SqlKind.OTHER);
  private static final SqlOperator UPPER = op("UPPER", SqlKind.OTHER_FUNCTION);
  private static final SqlOperator LOWER = op("LOWER", SqlKind.OTHER_FUNCTION);
  private static final SqlOperator ABS = op("ABS", SqlKind.OTHER_FUNCTION);
  private static final SqlOperator UNARY_PLUS = op("+", SqlKind.PLUS_PREFIX);
  private static final SqlOperator UNARY_MINUS = op("-", SqlKind.MINUS_PREFIX);
  private static final SqlOperator PLUS = op("+", SqlKind.PLUS);
  private static final SqlOperator MINUS = op("-", SqlKind.MINUS);
  private static final SqlOperator MULTIPLY = op("*", SqlKind.TIMES);
  private static final SqlOperator DIVIDE = op("/", SqlKind.DIVIDE);

  private static final Map<String, SqlAggFunction> AGGREGATES = new HashMap<>();

  static {
    for (String name : new String[] {"COUNT", "SUM", "AVG", "MAX", "MIN", "VAR_POP", "VAR_SAMP",
        "STDDEV_POP", "STDDEV_SAMP", "SINGLE_VALUE"}) {
      // Every one of these has name == kind name in Calcite's standard table, which is what the
      // translator's `case COUNT:` ... dispatch and its UFunc naming both key on.
      AGGREGATES.put(name, aggFn(name, SqlKind.valueOf(name)));
    }
  }

  /** Operators whose meaning is the same on both sides. Everything else is minted, not guessed. */
  private SqlOperator standard(String op, int arity) {
    switch (op) {
      case "=": return EQUALS;
      case "<>": return NOT_EQUALS;
      case "<": return LESS_THAN;
      case "<=": return LESS_THAN_OR_EQUAL;
      case ">": return GREATER_THAN;
      case ">=": return GREATER_THAN_OR_EQUAL;
      case "AND": return AND;
      case "OR": return OR;
      case "NOT": return NOT;
      case "IS NULL": return IS_NULL;
      case "IS NOT NULL": return IS_NOT_NULL;
      case "IS DISTINCT FROM": return IS_DISTINCT_FROM;
      case "IS NOT TRUE": return IS_NOT_TRUE;
      case "CASE": return CASE;
      case "COALESCE": return COALESCE;
      case "NULLIF": return NULLIF;
      case "LIKE": return LIKE;
      case "||": return CONCAT;
      case "UPPER": return UPPER;
      case "LOWER": return LOWER;
      case "ABS": return ABS;
      case "+": return arity == 1 ? UNARY_PLUS : PLUS;
      case "-": return arity == 1 ? UNARY_MINUS : MINUS;
      case "*": return MULTIPLY;
      case "/": return DIVIDE;
      default: return null;
    }
  }

  /**
   * An uninterpreted function keyed by name, which their translator turns into a {@code UFunc}.
   *
   * <p>Under Calcite this had to be a {@code SqlFunction} with a return-type inference and an
   * operand checker, because the operator might have been asked to derive its own type. Here the
   * type is supplied explicitly at every call site, so the operator only has to carry a name and a
   * kind -- which is all the translator ever reads off it.
   */
  private SqlOperator mint(String name) {
    return minted.computeIfAbsent(name, k -> op(k, SqlKind.OTHER_FUNCTION));
  }

  private RelDataType type(String t) {
    final SqlTypeName n;
    switch (t) {
      case "INTEGER": n = SqlTypeName.INTEGER; break;
      // The temporal types the frontend keeps apart: each is exact as an integer in its own unit,
      // and it never lets two of them meet except through a `q_conv_*` call, which `mint` turns into
      // an uninterpreted function like any other unknown operator. INTERVAL is not linear in one
      // unit (months), so it stays opaque, like VARBINARY.
      case "DATE": case "TIME": case "TIMESTAMP": n = SqlTypeName.INTEGER; break;
      case "INTERVAL": n = SqlTypeName.VARCHAR; break;
      case "BOOLEAN": n = SqlTypeName.BOOLEAN; break;
      case "REAL": n = SqlTypeName.REAL; break;
      case "VARCHAR": n = SqlTypeName.VARCHAR; break;
      case "VARBINARY": n = SqlTypeName.VARCHAR; break; // opaque catch-all; see `literal`
      default: n = SqlTypeName.ANY; break;
    }
    final RelDataType base =
        n == SqlTypeName.VARCHAR ? RelDataType.sql(n, 65536) : RelDataType.sql(n);
    return base.withNullability(true);
  }
}
