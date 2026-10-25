# The release build image (release/README.md, "Moving the image forward").

# The GNU Arm toolchain the UNA SDK builds with, by digest because a tag can be pushed again.
FROM xanderhendriks/stm32cubeide:16.0@sha256:7e07c508e3944def22eaabe822eaf902a2ba4fbb38a3ce24d6ff874f9f04c447

# Ubuntu's archive as it stood at this moment (snapshot.ubuntu.com), so that apt resolves
# the same package versions whenever the image is built.
ARG UBUNTU_SNAPSHOT=20261020T000000Z
RUN echo "APT::Snapshot \"${UBUNTU_SNAPSHOT}\";" > /etc/apt/apt.conf.d/50snapshot \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      git curl ca-certificates cmake make \
      python3 python3-pyelftools python3-pil \
 && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /opt/arm/bin \
 && for tool in arm-none-eabi-gcc arm-none-eabi-g++ arm-none-eabi-objcopy arm-none-eabi-size arm-none-eabi-objdump; do \
      path="$(find /opt/st -type f -name "$tool" | head -1)"; \
      test -n "$path" && chmod +x "$path" && ln -s "$path" /opt/arm/bin/"$tool"; \
    done

# A fixed locale, so that nothing sorts or decodes by the host's.
ENV LANG=C.UTF-8 LC_ALL=C.UTF-8 \
    RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/arm/bin:$PATH

# rustup itself, by version and checksum rather than whatever sh.rustup.rs serves today.
ARG RUSTUP_VERSION=1.29.2
ARG RUSTUP_SHA256=5be191e8c70a5007074254cb74d78dbfdd25cd5a61c27de7b8e5410122ec24f4
# toolchain.json's toolchains (tools/test-release.py checks), readable by every user a build runs as.
RUN curl --proto '=https' --tlsv1.2 -sSfo /tmp/rustup-init \
      "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init" \
 && echo "${RUSTUP_SHA256}  /tmp/rustup-init" | sha256sum -c - \
 && chmod +x /tmp/rustup-init \
 && /tmp/rustup-init -y --no-modify-path --profile minimal \
      --default-toolchain 1.99.1 --component clippy --target thumbv8m.main-none-eabihf \
 && rm /tmp/rustup-init \
 && rustup toolchain install nightly-2026-10-19 --profile minimal --component rust-src \
 && chmod -R a+rX /opt/rustup /opt/cargo

WORKDIR /src

LABEL org.opencontainers.image.source=https://github.com/tobymurray/wrist-rs \
      org.opencontainers.image.description="GNU Arm toolchain, CMake and pinned Rust, for building wrist-rs releases" \
      org.opencontainers.image.licenses=AGPL-3.0-or-later
