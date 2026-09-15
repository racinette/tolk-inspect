# Security policy

## Supported versions

The latest published `0.1.x` release receives security fixes. Unreleased source snapshots
and older `0.1.x` releases are not supported once a replacement is available.

## Reporting a vulnerability

Please report vulnerabilities privately through
[GitHub Security Advisories](https://github.com/racinette/tolk-inspect/security/advisories/new).
Include the affected version, a minimal reproduction, the expected impact, and any known
mitigations. Please do not open a public issue before the report has been assessed.

The project is a read-only analyzer, but its inputs are untrusted source text. Reports about
crashes, excessive resource consumption, path confusion, malformed WASM/package artifacts,
or incorrect security-analysis results are all in scope.
