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
parameters, function returns, recursion, nested tuple/object fields, and array/map values.
Each lambda has a stable synthetic `SymbolInfo`, so callers can navigate its target like any
other symbol.
Calls inside a lambda use that synthetic symbol as their `caller`. Captured callable
values are snapshotted at the lambda's creation point, matching Tolk's by-value closure
semantics, and nested-lambda captures propagate transitively.

The call graph is intentionally a whole-program may-call graph. For example, if
`invoke(fn) { fn() }` is called once with `allow` and once with `reject`, the call inside
`invoke` reports both targets. Without a particular invocation context, the safe answer
is the union of every reachable callee rather than a per-invocation result.

## Arrays and maps

Standard collection operations participate in the same CFG and whole-program fixed point.
Array literals preserve exact positions; `get`, `first`, `last`, `set`, `push`, and `pop` carry
callable values. Map lookup and mutation operations preserve values by key, including
`setAndGetPrevious`, `replaceAndGetPrevious`, `addOrGetExisting`, and
`deleteAndGetDeleted`. Ordered entry operations such as `findFirst` and `iterateNext` return
the conservative union of callable values stored in the map.

Literal and evaluated constant indexes/keys select one tracked entry. A dynamic index/key
returns the union of all entries it may select, while a dynamic write is conservatively
included in every compatible later lookup. Collection values and mutations also propagate
through function parameters, returns, branches, loops, and `mutate` helper parameters.

`createEmptyMap`, `createEmptyTuple`, `toLowLevelDict`/`createMapFromLowLevelDict`, and
`toTuple`/`fromTuple` are also modeled. Packing an aggregate into a low-level tuple intentionally
widens field identity: unpacked fields may contain any callable slot from the packed value, while
remaining complete when all slots were known.

Source-defined higher-order helpers require no builtin model. For example, Acton's
`array.each`, `array.map`, and `array.filter` flow through their ordinary receiver, callback
parameter, callback return, loop, and collection operations. Generic instance calls preserve the
receiver slot, and tuple/tensor destructuring distributes nested callable values to each binding.
The stdlib's nested-tuple `lisp_list` representation also preserves callable heads across literal
casts, prepend, lookup, tail, and pop operations.

```tolk
fun invoke(index: int, value: int): int {
    var handlers: array<(int) -> int> = [allow, reject];
    handlers.push(review);

    val exact = handlers.get(0);     // target: allow
    val selected = handlers.get(index); // targets: allow, reject, review
    return exact(selected(value));
}
```

The analysis always computes the internal CFGs it needs. Setting `controlFlow: "none"`
only omits public CFG objects and does not reduce call-site or call-graph precision.

The audited operation matrix and the boundary for genuinely opaque runtime values are documented
in [callable-flow audit](callable-flow-audit.md).
