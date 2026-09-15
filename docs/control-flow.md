# Control-flow analysis

`tolk-inspect` exposes a control-flow graph (CFG) for each analyzed function, method, and
get-method. Unlike the project call graph, which connects callers to callees, a CFG models
the possible execution paths between expressions and statements inside one callable.

CFG generation is controlled when constructing the project:

```ts
const project = await inspectProject({
  root: "/project",
  files,
  controlFlow: "workspace", // default; use "none" or "all" when appropriate
});
```

`"workspace"` excludes standard-library and Acton-library functions to keep the immutable
snapshot reasonably small. `"all"` includes every indexed source file.

## Public API

```ts
type ControlFlowNodeId = string;

interface ControlFlowNode {
  readonly id: ControlFlowNodeId;
  readonly kind:
    | "entry" | "exit" | "nop" | "expression" | "condition" | "assert"
    | "return" | "throw" | "break" | "continue" | "matchPattern"
    | "catchBinding" | "join";
  readonly location?: SourceLocation;
  readonly astNodeId?: NodeId;
  readonly reads: readonly SymbolId[];
  readonly writes: readonly SymbolId[];
}

interface ControlFlowEdge {
  readonly from: ControlFlowNodeId;
  readonly to: ControlFlowNodeId;
  readonly kind:
    | "unconditional" | "trueBranch" | "falseBranch" | "loopBack"
    | "break" | "continue" | "return" | "throw" | "exceptional";
}

class ControlFlowGraph {
  readonly symbolId: SymbolId;
  readonly entry: ControlFlowNodeId;
  readonly exit: ControlFlowNodeId;
  readonly nodes: readonly ControlFlowNode[];
  readonly edges: readonly ControlFlowEdge[];

  node(id: ControlFlowNodeId): ControlFlowNode | undefined;
  successors(node: ControlFlowNodeId | ControlFlowNode): readonly ControlFlowEdge[];
  predecessors(node: ControlFlowNodeId | ControlFlowNode): readonly ControlFlowEdge[];
  isReachable(node: ControlFlowNodeId | ControlFlowNode): boolean;
  reachableFrom(node: ControlFlowNodeId | ControlFlowNode): readonly ControlFlowNode[];
  dominates(required: ControlFlowNodeId | ControlFlowNode,
            target: ControlFlowNodeId | ControlFlowNode): boolean;
  postDominates(required: ControlFlowNodeId | ControlFlowNode,
                origin: ControlFlowNodeId | ControlFlowNode): boolean;
}

project.controlFlow(symbol: SymbolId | SymbolInfo): ControlFlowGraph | undefined;
project.controlFlowGraphs(): readonly ControlFlowGraph[];
```

Graph and node IDs are opaque and valid only within one project snapshot. Entry, exit,
join, and other synthetic nodes may have no source location or AST node. Read/write sets
contain the public IDs of local variables and parameters.

## User story: authorization before mutation

An auditor wants to ensure that every mutation of a mutable balance parameter is preceded
by an ownership assertion on every execution path.

```tolk
fun safeDebit(mutate balance: int, isOwner: bool, amount: int) {
    assert (isOwner) throw 401;
    balance -= amount;
}

fun unsafeDebit(mutate balance: int, isOwner: bool, amount: int) {
    if (amount > 0) {
        assert (isOwner) throw 401;
    }
    balance -= amount;
}
```

The rule finds ownership assertions through semantic resolution, finds CFG nodes that
write `balance`, and asks whether an assertion dominates each write. `A` dominates `B`
when every path from function entry to `B` passes through `A`.

```ts
for (const fn of project.symbols().filter((symbol) => symbol.kind === "function")) {
  const cfg = project.controlFlow(fn);
  if (!cfg) continue;

  const owned = project.symbols().filter((symbol) => symbol.containingSymbol === fn.id);
  const balance = owned.find((symbol) => symbol.name === "balance" && symbol.flags.mutable);
  const isOwner = owned.find((symbol) => symbol.name === "isOwner");
  if (!balance || !isOwner) continue;

  const checks = cfg.nodes.filter((node) => {
    if (node.kind !== "assert" || !node.astNodeId) return false;
    const ast = project.node(node.astNodeId);
    return ast !== undefined && [ast, ...ast.descendants()].some(
      (child) => project.resolve(child)?.symbolId === isOwner.id,
    );
  });

  for (const write of cfg.nodes.filter((node) => node.writes.includes(balance.id))) {
    if (!checks.some((check) => cfg.dominates(check, write))) {
      console.error(`${fn.name}: balance mutation is not guarded`, write.location);
    }
  }
}
```

This reports `unsafeDebit`. Merely finding an assertion and an assignment in the same AST
would be insufficient because the assertion could occur after the assignment or only in
another branch.

## Common helper use cases

### `successors` and `predecessors`

Use `successors(condition)` to inspect the true and false branches leaving a condition.
Use `predecessors(node)` to explain which branches, loop iterations, or exceptional paths
can reach a finding.

### `isReachable` and `reachableFrom`

`isReachable(node)` detects nodes that have no path from function entry. `reachableFrom`
includes its starting node and is useful for asking which state changes, throws, returns,
or calls may happen after a sensitive operation.

### `dominates`

`dominates(required, target)` proves that `required` occurs on every entry-to-target path.
Typical uses include authorization before state mutation, bounds checks before indexing,
nonzero checks before division, and initialization before a read.

Dominance proves ordering, not that the check remains valid. If code can modify inputs
used by a check, a rule must additionally propagate validity state through the graph.

### `postDominates`

`postDominates(required, origin)` proves that every terminating path from `origin` to the
function exit passes through `required`. It is useful for mandatory cleanup, persistence,
logging, and finalization after a resource is acquired or state is changed.

The helper considers terminating paths. A reachable infinite loop with no path to exit
does not itself disprove post-dominance when another terminating path exists. The helper
returns `false` when the origin itself has no path to the exit.

## Deliberate limits

The first public CFG surface contains structural control flow, locations, AST links, and
local read/write sets. Acton's internal audit-specific taint facts are not yet a public
contract. `tolk-inspect` uses CFGs internally for conservative call-target analysis, but
the resulting targets are exposed through `project.callSites()` and the call graph rather
than as CFG-node facts.
