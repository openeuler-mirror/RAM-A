# Open Source Software Notice

This file records the third-party open source software notice for ram-a-kv.

ram-a-kv itself is licensed under Mulan PSL v2. See [LICENSE](../LICENSE) for
the project license text.

## Scope

This is an initial notice generated from the current Cargo workspace dependency
set. It is intended to make the third-party dependency notice explicit for
review and future maintenance.

The current workspace uses Rust crates managed by Cargo. Direct third-party
dependencies are listed below. Transitive dependencies are resolved through
`Cargo.lock`; the current resolved dependency graph contains 187 third-party
crate packages.

The `openclaw-plugin/` TypeScript plugin has no runtime npm dependencies. It
declares `typescript` (Apache-2.0) as a build-time devDependency and `openclaw`
as a peerDependency provided by the host application; neither is bundled into
this repository.

When dependencies change, or before producing a release package or binary
distribution, this notice should be regenerated or expanded from `Cargo.lock`.
Release-oriented notices may also need exact copyright notices and full license
texts for every third-party component.

## Direct Third-Party Dependencies

| Package | Version | Used by | License |
| --- | --- | --- | --- |
| async-trait | 0.1.89 | ram-a-kv, manager-core | MIT OR Apache-2.0 |
| axum | 0.7.9 | ram-a-kv | MIT |
| dirs | 5.0.1 | ram-a-kv-sdk | MIT OR Apache-2.0 |
| reqwest | 0.12.28 | ram-a-kv, ram-a-kv-sdk | MIT OR Apache-2.0 |
| rusqlite | 0.32.1 | ram-a-kv | MIT |
| serde | 1.0.228 | ram-a-kv, manager-core, ram-a-kv-sdk | MIT OR Apache-2.0 |
| serde_json | 1.0.150 | ram-a-kv, manager-core, ram-a-kv-sdk | MIT OR Apache-2.0 |
| tempfile | 3.27.0 | manager-core tests | MIT OR Apache-2.0 |
| thiserror | 1.0.69 | ram-a-kv, manager-core | MIT OR Apache-2.0 |
| tokio | 1.52.3 | ram-a-kv, manager-core, ram-a-kv-sdk | MIT |
| toml | 0.8.23 | ram-a-kv, manager-core, ram-a-kv-sdk | MIT OR Apache-2.0 |
| tracing | 0.1.44 | ram-a-kv, manager-core, ram-a-kv-sdk | MIT |
| tracing-subscriber | 0.3.23 | ram-a-kv | MIT |

## Resolved License Families

The current transitive dependency graph contains the following license
expressions according to Cargo package metadata:

| License expression | Package count |
| --- | ---: |
| (MIT OR Apache-2.0) AND Unicode-3.0 | 1 |
| Apache-2.0 | 1 |
| Apache-2.0 AND ISC | 1 |
| Apache-2.0 OR BSL-1.0 | 1 |
| Apache-2.0 OR ISC OR MIT | 2 |
| Apache-2.0 OR MIT | 9 |
| Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | 3 |
| BSD-2-Clause OR Apache-2.0 OR MIT | 2 |
| BSD-3-Clause | 1 |
| CDLA-Permissive-2.0 | 1 |
| ISC | 2 |
| MIT | 33 |
| MIT AND BSD-3-Clause | 1 |
| MIT OR Apache-2.0 | 100 |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later | 1 |
| MIT OR Apache-2.0 OR Zlib | 2 |
| MIT/Apache-2.0 | 5 |
| MPL-2.0 | 1 |
| Unicode-3.0 | 18 |
| Unlicense OR MIT | 1 |
| Zlib OR Apache-2.0 OR MIT | 1 |

No resolved third-party crate in the current Cargo metadata snapshot is missing
a declared license field.
