# Call-site and call-graph analysis

`tolk-inspect` records every direct call and every indirect call through a local callable
that belongs to a function CFG. Indirect targets are a conservative may-call set: if a
call can reach either of two functions on different paths, both functions are reported.

## Public API

```ts
interface CallSite {
  readonly caller: SymbolId;
  readonly location: SourceLocation;
  readonly nodeId?: NodeId;
  readonly dispatch: "direct" | "indirect";
  readonly targets: readonly SymbolId[];
  readonly complete: boolean;
}

interface CallEdge {
  readonly caller: SymbolId;
  readonly callee: SymbolId;
  readonly callSite: SourceLocation;
  readonly nodeId?: NodeId;
  readonly dispatch: "direct" | "indirect";
}

project.callSites(): readonly CallSite[];
project.callSites(caller: SymbolId | SymbolInfo): readonly CallSite[];
project.callGraph(): readonly CallEdge[];
project.calls(caller: SymbolId | SymbolInfo): readonly CallEdge[];
project.callers(callee: SymbolId | SymbolInfo): readonly CallEdge[];
```

`callSites()` preserves calls that have no known global target. The edge APIs flatten
each known target into one edge, so a wholly unknown call produces no edge.

## Example: branch-selected handler

```tolk
fun allow(value: int): int { return value; }
fun reject(value: int): int { throw 403; }

fun dispatch(shouldReject: bool, value: int): int {
    var handler = allow;
    if (shouldReject) {
        handler = reject;
    }
    return handler(value);
}
```

The indirect call has both `allow` and `reject` in `targets` and has `complete: true`.
An audit can therefore find all dispatch paths without manually interpreting assignments:

```ts
const reject = project.symbols().find((symbol) => symbol.name === "reject")!;

for (const callSite of project.callSites()) {
  if (callSite.targets.includes(reject.id)) {
    console.log("may reject", callSite.location);
  }
  if (!callSite.complete) {
    console.log("requires manual review: unknown target is also possible", callSite.location);
  }
}
```

## Completeness

- `complete: true` means `targets` contains every target represented by the modeled local
  value flow. It does not mean every listed CFG path is feasible at runtime.
- `complete: false` with targets means the call has known targets and may also have an
  unknown target.
- `complete: false` with no targets means no global target could be established.

The analysis follows direct function, get-method, and resolved method references through
simple assignments, parenthesized values, function-type casts, explicit generic
instantiations, local copies, ternary expressions, branches, loops, and exceptional paths.
It computes loop results to a fixed point and preserves the previous value along an
assignment's exceptional edge. Callback parameters, values returned by calls, lambdas,
and callable values extracted from containers currently introduce an unknown target.
Lambda bodies do not yet have independent callable symbols or CFGs.

The analysis always computes the internal CFGs it needs. Setting `controlFlow: "none"`
only omits public CFG objects and does not reduce call-site or call-graph precision.
