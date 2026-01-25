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

release: tests tag pkg debian-pkg pub-pkg

tests: run-tests clippy

run-tests:
    cargo test --features server

clippy:
    clippy --features cli,cluster

docker-cross:
	cd docker.cross && \
		docker build -t bmauto/psrt-cross-aarch64 . -f Dockerfile.rust.aarch64 && \
		docker build -t bmauto/psrt-cross-x86_64 . -f Dockerfile.rust.x86_64

pkg:
    rm -rf _build
    mkdir -p _build
    CARGO_TARGET_DIR=target-x86_64 cross build --target x86_64-unknown-linux-gnu --release --features server,cli
    CARGO_TARGET_DIR=target-aarch64 cross build --target aarch64-unknown-linux-gnu --release --features server,cli
    cd target-x86_64/x86_64-unknown-linux-gnu/release && \
      tar czvf ../../../_build/psrt-{{VERSION}}-x86_64.tar.gz psrtd psrt-cli
    cd target-aarch64/aarch64-unknown-linux-gnu/release && \
      tar czvf ../../../_build/psrt-{{VERSION}}-aarch64.tar.gz psrtd psrt-cli

debian-pkg:
    cd make-deb && TARGET_ARCH=aarch64 ./build.sh && mv psrt-{{VERSION}}-arm64.deb ../_build/
    cd make-deb && TARGET_ARCH=x86_64 ./build.sh && mv psrt-{{VERSION}}-amd64.deb ../_build/

pub-pkg:
    cd _build && echo "" | gh release create v{{VERSION}} -t "v{{VERSION}}" \
            psrt-{{VERSION}}-x86_64.tar.gz \
            psrt-{{VERSION}}-aarch64.tar.gz \
            psrt-{{VERSION}}-amd64.deb \
            psrt-{{VERSION}}-arm64.deb \
    cd ~/src/apt/repo && reprepro includedeb stable /opt/psrt/_build/psrt-{{VERSION}}-amd64.deb
    cd ~/src/apt/repo && reprepro includedeb stable /opt/psrt/_build/psrt-{{VERSION}}-arm64.deb
    cd ~/src/apt && just pub

release-enterprise:
    DOCKER_OPTS="-v /opt/eva4-enterprise:/opt/eva4-enterprise" cross build --target x86_64-unknown-linux-musl --release --features cli,cluster
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
