FROM xanderhendriks/stm32cubeide:16.0@sha256:7e07c508e3944def22eaabe822eaf902a2ba4fbb38a3ce24d6ff874f9f04c447
RUN apt-get update && apt-get install -y --no-install-recommends git curl ca-certificates cmake make python3 python3-pip python3-pyelftools python3-pil && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /opt/arm/bin && for tool in arm-none-eabi-gcc arm-none-eabi-g++ arm-none-eabi-objcopy arm-none-eabi-size arm-none-eabi-objdump; do p="$(find /opt/st -type f -name "$tool" | head -1)"; test -n "$p" && chmod +x "$p" && ln -s "$p" /opt/arm/bin/"$tool"; done
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/opt/arm/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.99.1 --component clippy --target thumbv8m.main-none-eabihf
RUN rustup toolchain install nightly-2026-10-19 --profile minimal --component rust-src
WORKDIR /src
