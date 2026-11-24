# Windows installer build in a container (run it with `node tooling/docker-build.mjs`).
#
# Why a container: a build on a developer machine records that machine's paths (home
# directory, user name) in the executable through panic messages and C `__FILE__` strings.
# Here the only user is `testerek`, the only home is /home/testerek and the source is /work,
# so nothing about the person who started the build can reach the artifacts. The source is
# streamed in as a tar of the committed tree, never bind-mounted from the host checkout, and
# the host user's name is not an argument of this image: only its numeric uid/gid, so files
# written to the output directory belong to the caller.
ARG RUST_IMAGE=rust:1.98.1-trixie
ARG NODE_IMAGE=node:24.18.0-trixie-slim

FROM ${NODE_IMAGE} AS node

FROM ${RUST_IMAGE}
ARG BUILD_USER=testerek
ARG BUILD_UID=1000
ARG BUILD_GID=1000
ARG CARGO_XWIN_VERSION=0.23.1
ARG PNPM_VERSION=10.34.5

# Host dependencies of a Linux -> Windows Tauri build: clang/lld/llvm for cargo-xwin, NSIS to
# package, appindicator because the Tauri CLI checks it even when packaging for Windows.
RUN apt-get update \
 && apt-get install --yes --no-install-recommends \
      clang lld llvm nsis libayatana-appindicator3-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*

COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && npm install --global "pnpm@${PNPM_VERSION}" \
 && npm cache clean --force

RUN rustup target add x86_64-pc-windows-msvc \
 && cargo install --locked --version "${CARGO_XWIN_VERSION}" cargo-xwin \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git

# --non-unique: the caller's numeric ids may already belong to a system account or group of the
# image (a gid of 100 is `users`); the build user is the neutral name, whatever the number.
RUN groupadd --non-unique --gid "${BUILD_GID}" "${BUILD_USER}" \
 && useradd --non-unique --uid "${BUILD_UID}" --gid "${BUILD_GID}" --create-home --shell /bin/bash "${BUILD_USER}" \
 && install -d -o "${BUILD_UID}" -g "${BUILD_GID}" \
      "/home/${BUILD_USER}/.cargo" "/home/${BUILD_USER}/.cache" \
      "/home/${BUILD_USER}/.local/share/pnpm" /work /work/target /out

# Toolchain stays where the official image put it (read-only for the build user); only the
# registry cache moves under the home directory, where the cache volumes are mounted.
ENV CARGO_HOME=/home/${BUILD_USER}/.cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:${PATH} \
    XWIN_ARCH=x86_64 \
    CI=true

COPY --chmod=0755 build-entrypoint.sh /usr/local/bin/rift-build
USER ${BUILD_USER}
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/rift-build"]
