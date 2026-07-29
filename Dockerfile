ARG version=1.93.0
FROM rust:${version}
ARG version

RUN dpkg --add-architecture arm64 && \
    apt-get update && \
    apt-get install -y --no-install-recommends \
      cmake \
      libclang-dev \
      pkg-config \
      libudev-dev \
      libudev-dev:arm64 \
      libinput-dev \
      libinput-dev:arm64 \
      libxkbcommon-dev \
      libxkbcommon-dev:arm64 \
      libssl-dev \
      libssl-dev:arm64 \
      libpulse-dev \
      libpulse-dev:arm64 \
      libwayland-dev \
      libwayland-dev:arm64 \
      libspa-0.2-dev \
      libspa-0.2-dev:arm64 \
      libpipewire-0.3-dev \
      libpipewire-0.3-dev:arm64 \
      libwireplumber-0.5-dev \
      libwireplumber-0.5-dev:arm64 \
      g++-aarch64-linux-gnu \
      libc6-dev-arm64-cross \
      rpm \
      librpmbuild10 \
      elfutils \
      curl \
      ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Taskfile support
RUN curl -1sLf \
      'https://dl.cloudsmith.io/public/task/task/setup.deb.sh' | bash && \
    apt-get update && \
    apt-get install -y --no-install-recommends task && \
    rm -rf /var/lib/apt/lists/*

# Install the ARM standard library for the actual Rust toolchain.
RUN rustup target add \
      --toolchain "${version}-x86_64-unknown-linux-gnu" \
      aarch64-unknown-linux-gnu && \
    rustup component add \
      --toolchain "${version}-x86_64-unknown-linux-gnu" \
      clippy

RUN chmod -R 777 /usr/local/rustup

ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
ENV CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
ENV CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++

ENV PKG_CONFIG_ALLOW_CROSS=1
ENV PKG_CONFIG_LIBDIR_aarch64_unknown_linux_gnu=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig
ENV PKG_CONFIG_LIBDIR_x86_64_unknown_linux_gnu=/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig