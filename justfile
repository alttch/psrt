VERSION := `grep ^version Cargo.toml|cut -d\" -f2`

all:
    @echo "select target"

clean:
    rm -rf _build
    cargo clean
    rm -rf target*

tag:
    git tag -a v{{VERSION}} -m v{{VERSION}}
    git push origin --tags

release: tests pub tag pkg debian-pkg pub-pkg

tests: run-tests clippy

run-tests:
    cargo test --features server

clippy:
    clippy --features cli,cluster

pub: publish-cargo-crate

publish-cargo-crate:
    cargo publish

pkg:
    lsb_release -cs|grep ^focal$
    rm -rf _build
    mkdir -p _build
    CARGO_TARGET_DIR=target-x86_64-musl cross build --target x86_64-unknown-linux-musl --release --features server,cli,openssl-vendored
    CARGO_TARGET_DIR=target-aarch64-musl cross build --target aarch64-unknown-linux-musl --release --features server,cli,openssl-vendored
    test $(lsb_release -r -s) = "20.04"
    cargo build --release --features server,cli
    cd target-x86_64-musl/x86_64-unknown-linux-musl/release && tar czvf ../../../_build/psrt-{{VERSION}}-x86_64-musl.tar.gz psrtd psrt-cli
    cd target-aarch64-musl/aarch64-unknown-linux-musl/release && \
            aarch64-linux-gnu-strip psrtd && \
            aarch64-linux-gnu-strip psrt-cli && \
            tar czvf ../../../_build/psrt-{{VERSION}}-aarch64-musl.tar.gz psrtd psrt-cli

debian-pkg:
    cd make-deb && TARGET_DIR=target-x86_64-musl ./build.sh && mv psrt-{{VERSION}}-amd64.deb ../_build/
    cd make-deb && PACKAGE_SUFFIX=-ubuntu20.04 RUST_TARGET=. ./build.sh && \
        mv psrt-{{VERSION}}-amd64-ubuntu20.04.deb ../_build/

pub-pkg:
    cd _build && echo "" | gh release create v{{VERSION}} -t "v{{VERSION}}" \
            psrt-{{VERSION}}-x86_64-musl.tar.gz \
            psrt-{{VERSION}}-aarch64-musl.tar.gz \
            psrt-{{VERSION}}-amd64.deb \
            psrt-{{VERSION}}-amd64-ubuntu20.04.deb
    cd /opt/apt/repo && reprepro includedeb stable /opt/psrt/_build/psrt-{{VERSION}}-amd64-ubuntu20.04.deb
    cd /opt/apt && just pub

release-enterprise:
    DOCKER_OPTS="-v /opt/eva4-enterprise:/opt/eva4-enterprise" cross build --target x86_64-unknown-linux-musl --release --features cli,cluster,openssl-vendored
    cargo build --release --features cluster,cli
    cd make-deb && \
        TARGET_DIR=target-x86_64-musl ./build.sh enterprise && \
      PACKAGE_SUFFIX=-ubuntu20.04 RUST_TARGET=. ./build.sh enterprise && \
        gsutil cp -a public-read psrt-enterprise-{{VERSION}}-amd64.deb gs://pub.bma.ai/psrt-enterprise/ && \
        gsutil cp -a public-read psrt-enterprise-{{VERSION}}-amd64-ubuntu20.04.deb gs://pub.bma.ai/psrt-enterprise/
    cd /opt/apt/repo && reprepro includedeb stable /opt/psrt/make-deb/psrt-enterprise-{{VERSION}}-amd64-ubuntu20.04.deb
    cd /opt/apt && just pub

launch-test-server *ARGS:
  cargo run --release --bin psrtd --features server -- --config ./test-configs/config.yml {{ARGS}}
launch-test-cserver *ARGS:
  cargo run --release --bin psrtd --features server,cluster -- --config ./test-configs/config.yml {{ARGS}}
launch-test-cserver2 *ARGS:
  cargo run --release --bin psrtd --features server,cluster -- --config ./test-configs/config2.yml {{ARGS}}
launch-test-benchmark *ARGS:
  cargo run --release --bin psrt-cli --features cli -- localhost:2873 --benchmark {{ARGS}}
launch-test-top *ARGS:
  cargo run --release --bin psrt-cli --features cli -- localhost:2873 -t '#' --top {{ARGS}}
launch-test-client *ARGS:
  cargo run --release --bin psrt-cli --features cli -- localhost:2873 {{ARGS}}
