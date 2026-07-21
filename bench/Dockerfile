FROM rust:1.91-bookworm

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends \
        clang \
        cmake \
        python3 \
        time \
        valgrind \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /work

ENTRYPOINT ["bench/e2e-overhead.sh"]
