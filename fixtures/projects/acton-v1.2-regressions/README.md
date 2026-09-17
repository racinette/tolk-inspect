# Acton v1.2.0 upgrade regressions

These are focused, project-authored analyzer fixtures, not copies of an official Acton
project or evidence of C++ compiler compatibility.

The match-statements fixture checks direct statements in match arms through parsing,
resolution, CFG generation, local reads/writes, and conservative callable propagation.
The parser-fixes fixture exercises the upgraded grammar's enum separators, empty/nested
tuple types, leading-pipe parenthesized types, and escaped triple-string delimiters.

Native Rust and public Node/WASM tests consume the same files. The existing upstream
Acton v1.1.0 corpus remains a separate backward-compatibility check.
