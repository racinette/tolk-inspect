# Semantic edge cases

Small purpose-built inputs complement the upstream Acton corpus:

- `a.tolk` / `b.tolk`: cyclic imports, cyclic calls, and direct recursion;
- `missing-import.tolk`: a location-bearing unresolved import diagnostic;
- `unicode-error.tolk`: recoverable syntax errors after non-BMP Unicode.

