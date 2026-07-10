# Open Source Software Notice

This file records the third-party open source software notice for RAM-A.

RAM-A itself is licensed under Mulan PSL v2. See [LICENSE](./LICENSE) for the
project license text.

## Scope

This is an initial notice generated from the current Cargo workspace dependency
set. It is intended to make the third-party dependency notice explicit for
review and future maintenance.

The current workspace uses Rust crates managed by Cargo. Direct third-party
dependencies are listed below. Transitive dependencies are resolved through
`Cargo.lock`; the current resolved dependency graph contains 199 third-party
crate packages.

When dependencies change, or before producing a release package or binary
distribution, this notice should be regenerated or expanded from `Cargo.lock`.
Release-oriented notices may also need exact copyright notices and full license
texts for every third-party component.

## Direct Third-Party Dependencies

| Package | Version | Used by | License |
| --- | --- | --- | --- |
| anyhow | 1.0.102 | memory-bench | MIT OR Apache-2.0 |
| async-trait | 0.1.89 | memory-core | MIT OR Apache-2.0 |
| clap | 4.6.1 | memory-bench | MIT OR Apache-2.0 |
| reqwest | 0.12.28 | memory-core | MIT OR Apache-2.0 |
| rusqlite | 0.32.1 | memory-core | MIT |
| serde | 1.0.228 | memory-core, memory-bench | MIT OR Apache-2.0 |
| serde_json | 1.0.149 | memory-core, memory-bench | MIT OR Apache-2.0 |
| sqlite-vec | 0.1.7-alpha.10 | memory-core | MIT/Apache-2.0 |
| tempfile | 3.27.0 | memory-core tests | MIT OR Apache-2.0 |
| thiserror | 1.0.69 | memory-core | MIT OR Apache-2.0 |
| tokio | 1.52.3 | memory-core, memory-bench | MIT |
| uuid | 1.23.1 | memory-core | Apache-2.0 OR MIT |

## Resolved License Families

The current transitive dependency graph contains the following license
expressions according to Cargo package metadata:

| License expression | Package count |
| --- | ---: |
| (Apache-2.0 OR MIT) AND BSD-3-Clause | 1 |
| (MIT OR Apache-2.0) AND Unicode-3.0 | 1 |
| Apache-2.0 | 2 |
| Apache-2.0 / MIT | 1 |
| Apache-2.0 AND ISC | 1 |
| Apache-2.0 OR BSL-1.0 | 1 |
| Apache-2.0 OR ISC OR MIT | 2 |
| Apache-2.0 OR MIT | 10 |
| Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | 15 |
| BSD-2-Clause OR Apache-2.0 OR MIT | 2 |
| BSD-3-Clause | 1 |
| ISC | 2 |
| MIT | 27 |
| MIT OR Apache-2.0 | 101 |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later | 1 |
| MIT/Apache-2.0 | 11 |
| Unicode-3.0 | 18 |
| Unlicense OR MIT | 1 |
| Zlib | 1 |

No resolved third-party crate in the current Cargo metadata snapshot is missing
a declared license field.
