# Acton v1.2.1 upgrade regressions

These are focused, project-authored analyzer fixtures for the Tolk 1.5 behavior that Acton
v1.2.1 introduced. They are not copies of an official Acton project or evidence of C++
compiler compatibility.

The loop-control fixture checks that `break` and `continue` through nested `repeat`,
`while`, and `match` bodies keep their loop-back edges and that statements after an
unconditional exit are unreachable in the CFG. The smart-casts fixture checks that a
nullable local narrows inside a null check and keeps its declared type outside it. The
bits-alias fixture checks that a declared `bytesN` alias of `bitsN` is a user-defined type
alias whose methods resolve, since Tolk 1.5 no longer has a builtin `bytesN`.

Native Rust and public Node/WASM tests consume the same files.
