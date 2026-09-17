# Callable-flow audit

This audit checks the callable-carrying surfaces present in the pinned Tolk 1.4.2 / Acton analyzer
stack. It covers the core stdlib, Acton's source library, and its lambda-focused integration tests
originally at commit `17654feb713c5824ee4cc0259b7be9b5f72898ba`.

The audit's regression suite is retained against stable Acton v1.2.0, commit
`16d49e1f6ad68d67072b95c77ad9175c34ad7e17`. The upgrade also adds a regression for callback
assignments and invocations inside direct match-arm control-flow statements. This
revalidation does not replace the original source-surface audit with a new exhaustive audit.

## Audited surfaces

| Surface | Result |
| --- | --- |
| Direct functions, methods, and get-methods | Exact direct target |
| Locals, branches, loops, recursion, parameters, and returns | Whole-program may-call fixed point |
| Lambdas and by-value captures | Stable lambda symbol and callable CFG |
| Tuple/tensor/object fields | Nested values preserved; destructuring assigns each slot |
| `array` builtins | `get`, `first`, `last`, `set`, `push`, and `pop` modeled |
| Source-defined array helpers | `each`, `map`, and `filter` handled interprocedurally, including generic instance calls |
| `lisp_list` | Literal casts plus `prependHead`, `getHead`, `getTail`, and `popHead` preserve the nested tuple representation |
| Direct map lookup | `get(...).loadValue()` and `mustGet` modeled by constant or dynamic key |
| Map writes | `set`, `setAndGetPrevious`, `replaceIfExists`, `replaceAndGetPrevious`, `addIfNotExists`, and `addOrGetExisting` modeled |
| Map deletion | `delete` and `deleteAndGetDeleted` update the map; the latter also preserves the deleted value |
| Ordered map lookup | `findFirst`, `findLast`, key-relative finds, `iterateNext`, and `iteratePrev` return a safe union of stored values |
| Constructors and representation conversions | Empty map/tuple constructors, map/dictionary round trips, and tuple packing/unpacking modeled |

Constant array indexes and map keys retain the corresponding slot. A dynamic index, dynamic key,
or ordered traversal returns a conservative union. These unions can contain extra targets, but a
`complete: true` result still contains every target represented by the modeled value flow.

Low-level tuple packing erases source field names. The analysis therefore unions callable stack
slots when the value is unpacked instead of guessing a field layout. A map converted directly to a
low-level dictionary and back retains its key/value shape; a dictionary arriving from outside the
analyzed program remains unknown.

## Remaining opaque boundary

`complete: false` is still expected when a callable originates outside the supplied source:

- a callback parameter on an externally callable entrypoint with no analyzed caller;
- a generic value returned by FFI, emulator, environment, or another native runtime operation;
- a callable claimed to have been loaded from opaque serialized cell or slice data; or
- a low-level container supplied externally rather than derived from a tracked value.

Those operations provide runtime data but no global source symbol. Inventing a target would make
the graph unsound. When known and unknown flows merge, `callSites()` retains the known targets and
sets `complete: false`.

The call graph records source call sites. If an opaque runtime receives a continuation and invokes
it internally, that invocation has no Tolk source call site and is not represented as a synthetic
edge. The source-level call into the runtime remains available as its normal direct edge.
