# Build image for `./build.sh --target linux --docker` (Linux packages from a Mac or any Docker host).
# Ubuntu 24.04 on purpose: a deb/AppImage built against its older webkit2gtk installs on newer systems too.
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config curl wget ca-certificates git file perl xz-utils \
      libwebkit2gtk-4.1-dev libgtk-3-dev libssl-dev libxdo-dev librsvg2-dev libayatana-appindicator3-dev \
      libfuse2 squashfs-tools desktop-file-utils rpm \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
    && cargo install tauri-cli --version '^2' --locked
# linuxdeploy (AppImage) cannot use FUSE in a container
ENV APPIMAGE_EXTRACT_AND_RUN=1 NO_STRIP=true
WORKDIR /src
