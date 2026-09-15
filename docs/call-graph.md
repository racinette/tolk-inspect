# Call-site and call-graph analysis

`tolk-inspect` records every direct call and every indirect call that belongs to a
callable CFG, including lambda bodies. Indirect targets are a conservative may-call set:
if a call can reach either of two callable symbols on different paths, both are reported.

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

`callSites()` preserves calls that have no known target. The edge APIs flatten
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

## Example: callback passed through a factory

```tolk
fun allow(value: int): int { return value; }
fun forward<T>(value: T): T { return value; }

fun run(value: int): int {
    val callback = forward(allow);
    return callback(value);
}
```

The analysis propagates `allow` into `forward`'s parameter, through its return value,
and back into `callback`. The final call is indirect, has `targets: [allow.id]`, and is
complete. Recursive forwarding helpers are solved to the same fixed point.

## Completeness

- `complete: true` means `targets` contains every target represented by the modeled local
  value flow. It does not mean every listed CFG path is feasible at runtime.
- `complete: false` with targets means the call has known targets and may also have an
  unknown target.
- `complete: false` with no targets means no callable target could be established.

The analysis follows direct function, get-method, resolved method, and lambda references
through assignments, wrappers, local copies, ternaries, branches, loops, and exceptional
paths. A whole-program fixed point propagates callable values through arguments, callback
parameters, function returns, recursion, and nested tuple/object fields. Each lambda has a
stable synthetic `SymbolInfo`, so callers can navigate its target like any other symbol.
Calls inside a lambda use that synthetic symbol as their `caller`. Captured callable
values are snapshotted at the lambda's creation point, matching Tolk's by-value closure
semantics, and nested-lambda captures propagate transitively.

The analysis is context-insensitive: all calls to one function contribute to the same
parameter and return summaries. This deliberately produces a safe union of targets rather
than a per-caller result.

The analysis always computes the internal CFGs it needs. Setting `controlFlow: "none"`
only omits public CFG objects and does not reduce call-site or call-graph precision.
