ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ARG GLIBC_BASELINE
ARG RUST_TOOLCHAIN

# manylinux already supplies the native tools and CPython 3.13; Ubuntu needs them.
RUN if command -v apt-get >/dev/null; then \
      apt-get update && \
      DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential pkg-config libssl-dev cmake curl git ca-certificates \
        python3 python3-venv binutils && \
      rm -rf /var/lib/apt/lists/*; \
    fi && test "$(getconf GNU_LIBC_VERSION)" = "glibc ${GLIBC_BASELINE}"

ENV RUSTUP_HOME=/opt/rustup
ENV CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:/opt/python/cp313-cp313/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 --retry 3 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh && \
    sh /tmp/rustup-init.sh -y --no-modify-path --profile minimal \
      --default-toolchain "${RUST_TOOLCHAIN}" \
      --component clippy,rustfmt,rust-analyzer,rust-src && \
    chmod -R a+rX /opt/rustup /opt/cargo

# Bundle native dependencies while retaining dynamic glibc and its system loader.
ENV LIBGIT2_NO_PKG_CONFIG=1
ENV LIBZ_SYS_STATIC=1
ENV PCRE2_SYS_STATIC=1
ENV PYTHONUTF8=1
ENV PYTHONIOENCODING=utf-8
COPY run.sh /opt/vtcode-release.sh
ENTRYPOINT ["bash", "/opt/vtcode-release.sh"]
