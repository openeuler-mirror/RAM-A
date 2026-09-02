# ARM64 xiaoO + RAM-A Ready-to-Run Image Design

## Goal

Produce a self-contained `linux/arm64` openEuler 24.03 LTS SP3 image that installs the latest published xiaoO aarch64 RPM, builds RAM-A from AtomGit pull request 18 at its fetched head, embeds the requested provider credentials, and can immediately run and verify the `ram-a-mem` ingestion pipeline.

## Fixed inputs

- Base operating system: openEuler 24.03 LTS SP3 for `linux/arm64`.
- xiaoO package source: the public EulerMaker `openEuler-24.03-LTS-SP3:epol` aarch64 package repository.
- RAM-A source: AtomGit `openeuler/RAM-A` pull request 18, currently fetched at commit `c7ad5d0`.
- Personal-memory embedding provider: deterministic local `hash`, 1024 dimensions.
- Extraction and verification model: GLM-5.2 through the GLM Coding Plan OpenAI-compatible endpoint.
- xiaoO model provider: GLM-5.2 through the GLM Coding Plan Anthropic-compatible endpoint.
- Reranking and graph memory remain disabled for the initial ingestion acceptance test.
- GLM Coding Plan and OpenRouter credentials are intentionally embedded in the image environment at the user's request.

## Image architecture

The image is built for `linux/arm64` from an openEuler 24.03 LTS SP3 base. Docker Desktop may execute the build through ARM emulation on an amd64 host. Compilation is slower under emulation, but both the installed RPM and the RAM-A executable share the target architecture and runtime libraries.

The build has three logical phases:

1. Resolve and download the latest non-debug xiaoO aarch64 RPM from the provided EulerMaker repository index. Record the RPM filename, NEVRA, source URL, and SHA-256.
2. Install xiaoO and build `ram-a-mem` in release mode from the exact PR 18 commit copied into the build context.
3. Assemble runtime configuration, persistent-data directories, credentials, and operational scripts into the final image.

The old amd64 `xiaoo:latest` image is retained until the new image passes all acceptance checks. It may then be removed because the user explicitly requested replacement after successful validation.

## Runtime configuration

RAM-A listens on `0.0.0.0:18081` inside the container and uses:

- hash embeddings for personal memories and case-library indexing;
- GLM-5.2 for extraction and grounding;
- SQLite files under `/var/lib/ram-a`;
- bearer-token authentication between xiaoO and RAM-A;
- structured stage logging under `/var/log/ram-a`;
- graph memory, rerank, and optional case-summary LLM calls disabled during the first acceptance run.

xiaoO uses the bundled MCP configuration to reach `http://127.0.0.1:18081/mcp` and enables automatic pre-turn recall and post-turn ingestion.

## Bundled scripts

The image contains focused scripts under `/opt/ram-a/scripts`:

- `start-ram-a.sh`: validate configuration and launch `ram-a-mem`.
- `start-xiaoo.sh`: launch xiaoO with the bundled GLM and MCP configuration.
- `start-all.sh`: start RAM-A, wait for health, then start xiaoO.
- `healthcheck.sh`: check the RAM-A HTTP health endpoint.
- `verify-ingest.sh`: perform authenticated MCP initialization, invoke `memory_ingest`, repeat the request to verify idempotency, invoke `memory_search`, and check stage logs.
- `rebuild-ram-a.sh`: fetch PR 18, reset a disposable source checkout to the fetched PR head, rebuild release binaries, and install them in the container. It never mutates a user-mounted source checkout.
- `image-info.sh`: print architecture, xiaoO RPM NEVRA and checksum, RAM-A commit, and executable versions without printing credentials.

Host-side helper scripts build the image for `linux/arm64`, start it with persistent volumes, and rerun the acceptance test.

## Credential handling

The two provider credentials are supplied as build arguments and materialized as final image environment variables. They are not committed to the Git repository or printed by scripts, build logs, diagnostics, or verification output. Because image environment variables and layers are inspectable, possession of the image is equivalent to possession of the credentials.

## Failure handling

- RPM resolution fails if no main `xiaoO-*.aarch64.rpm` is present or if multiple candidates cannot be ordered by RPM version semantics.
- The build stops if the downloaded RPM metadata does not report aarch64 or if installation fails.
- The RAM-A build records and checks the expected PR 18 commit.
- Startup fails before launching services if required configuration, credentials, binaries, or writable data directories are missing.
- Verification stores redacted JSON responses and structured logs without provider secrets or raw authorization headers.
- The old image is not removed unless every mandatory acceptance check succeeds.

## Acceptance criteria

The result is accepted only when all of the following are demonstrated from the final image:

1. `uname -m` reports `aarch64` and the installed xiaoO package reports the latest repository NEVRA.
2. `ram-a-mem` is the release binary built from PR 18 head `c7ad5d0`, unless PR 18 advances before the build, in which case the newly fetched head is recorded and used.
3. RAM-A starts with hash embeddings and GLM-5.2 provider connectivity.
4. `memory_ingest` returns successful structured content and at least one memory ID for a candidate user message.
5. Repeating the same idempotent request returns the same memory IDs without duplicate persistence.
6. `memory_search` retrieves the ingested marker in the same principal scope.
7. Structured logs contain the canonical completed ingestion stages and do not contain credentials.
8. The final image contains the operational scripts and starts successfully with persistent data volumes.

