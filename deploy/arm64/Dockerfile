# syntax=docker/dockerfile:1.7

FROM --platform=linux/arm64 openeuler/openeuler:24.03-lts-sp3 AS builder

RUN dnf install -y \
      ca-certificates cargo curl gcc gcc-c++ git make openssl-devel \
      pkgconf-pkg-config rust sqlite-devel \
    && dnf clean all

WORKDIR /opt/RAM-A
RUN git init . \
    && git remote add upstream https://atomgit.com/openeuler/RAM-A.git \
    && git fetch --depth 1 upstream refs/merge-requests/18/head \
    && git checkout -B pr18 FETCH_HEAD \
    && git rev-parse HEAD >/tmp/ram-a-commit

RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/opt/RAM-A/ram-a-mem/target \
    cd ram-a-mem \
    && cargo test --locked -p memory-pipeline --test offline_pipeline \
    && cargo build --release --locked -p memory-mcp \
    && cp target/release/ram-a-mem /tmp/ram-a-mem

FROM --platform=linux/arm64 openeuler/openeuler:24.03-lts-sp3

ARG GLM_CODING_TOKEN
ARG OPENROUTER_API_KEY
ARG XIAOO_PACKAGES_URL="https://eulermaker.openeuler.openatom.cn/api/ems5/repositories/openEuler-24.03-LTS-SP3:epol/openEuler%3A24.03-LTS-SP3/aarch64/Packages/"

ENV GLM_CODING_TOKEN=${GLM_CODING_TOKEN} \
    ANTHROPIC_AUTH_TOKEN=${GLM_CODING_TOKEN} \
    ANTHROPIC_BASE_URL=https://open.bigmodel.cn/api/anthropic \
    OPENROUTER_API_KEY=${OPENROUTER_API_KEY} \
    RAM_A_XIAOO_TOKEN=ram-a-local-image-token \
    RAM_A_MEM_CONFIG=/etc/ram-a/ram-a-mem.json \
    RUST_LOG=info

RUN test -n "$GLM_CODING_TOKEN" && test -n "$OPENROUTER_API_KEY"

RUN dnf install -y \
      ca-certificates cargo curl gcc gcc-c++ git jq make openssl openssl-devel \
      pkgconf-pkg-config procps-ng python3 rpm rust sqlite tar xclip \
    && dnf clean all

RUN set -euo pipefail; \
    rpm_name="$(curl -fsSL "$XIAOO_PACKAGES_URL" \
      | sed -n 's/.*href="\(xiaoO-[0-9][^"]*\.aarch64\.rpm\)".*/\1/p' \
      | sort -V | tail -n 1)"; \
    test -n "$rpm_name"; \
    curl -fL --retry 3 "${XIAOO_PACKAGES_URL}${rpm_name}" -o /tmp/xiaoo.rpm; \
    test "$(rpm -qp --qf '%{ARCH}' /tmp/xiaoo.rpm)" = "aarch64"; \
    dnf install -y /tmp/xiaoo.rpm; \
    install -d /opt/metadata; \
    cp /tmp/xiaoo.rpm /opt/metadata/xiaoo.rpm; \
    rpm -q xiaoO >/opt/metadata/xiaoo-nevra; \
    sha256sum /tmp/xiaoo.rpm >/opt/metadata/xiaoo.sha256; \
    dnf clean all; \
    rm -f /tmp/xiaoo.rpm

COPY --from=builder /tmp/ram-a-mem /usr/local/bin/ram-a-mem
COPY --from=builder /tmp/ram-a-commit /opt/metadata/ram-a-commit
COPY --from=builder /opt/RAM-A /opt/RAM-A
COPY config/ram-a-mem.json /etc/ram-a/ram-a-mem.json
COPY config/xiaoo.toml /etc/xiaoo/config.toml
COPY config/xiaoo.mcp.json /etc/xiaoo/mcp.json
COPY scripts/ /opt/ram-a/scripts/

RUN chmod 0755 /opt/ram-a/scripts/*.sh /usr/local/bin/ram-a-mem \
    && chmod 0600 /etc/xiaoo/config.toml /etc/xiaoo/mcp.json \
    && chmod 0640 /etc/ram-a/ram-a-mem.json \
    && install -d -m 0750 /var/lib/ram-a /var/log/ram-a /run/ram-a \
    && install -d -m 0700 /var/lib/xiaoo

VOLUME ["/var/lib/ram-a", "/var/lib/xiaoo"]
EXPOSE 18081
HEALTHCHECK --interval=10s --timeout=5s --start-period=120s --retries=6 CMD ["/opt/ram-a/scripts/healthcheck.sh"]
ENTRYPOINT ["/opt/ram-a/scripts/start-all.sh"]
CMD []
