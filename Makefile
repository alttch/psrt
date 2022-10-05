VERSION=$(shell grep ^version Cargo.toml|cut -d\" -f2)

all:
	@echo "select target"

clean:
	rm -rf _build
	cargo clean

tag:
	git tag -a v${VERSION} -m v${VERSION}
	git push origin --tags

release: tests pub tag pkg debian-pkg pub-pkg

tests: run-tests clippy

run-tests:
	cargo test --features server

clippy:
	clippy --features server,cli,cluster

pub: publish-cargo-crate

publish-cargo-crate:
	cargo publish

pkg:
	rm -rf _build
	mkdir -p _build
	cross build --target x86_64-unknown-linux-musl --release --features server,cli,openssl-vendored
	cross build --target aarch64-unknown-linux-musl --release --features server,cli,openssl-vendored
	cd target/x86_64-unknown-linux-musl/release && tar czvf ../../../_build/psrt-${VERSION}-x86_64-musl.tar.gz psrtd psrt-cli
	cd target/aarch64-unknown-linux-musl/release && \
			aarch64-linux-gnu-strip psrtd && \
			aarch64-linux-gnu-strip psrt-cli && \
			tar czvf ../../../_build/psrt-${VERSION}-aarch64-musl.tar.gz psrtd psrt-cli

debian-pkg:
	cd make-deb && ./build.sh && mv psrt-${VERSION}-amd64.deb ../_build/

pub-pkg:
	cd _build && echo "" | gh release create v$(VERSION) -t "v$(VERSION)" \
			psrt-${VERSION}-x86_64-musl.tar.gz \
			psrt-${VERSION}-aarch64-musl.tar.gz \
			psrt-${VERSION}-amd64.deb

release-enterprise:
	cargo build --target x86_64-unknown-linux-musl --release --features server,cli,cluster,openssl-vendored
	cd make-deb && \
		./build.sh enterprise && \
		gsutil cp -a public-read psrt-enterprise-${VERSION}-amd64.deb gs://pub.bma.ai/psrt-enterprise/
	jks build pub.bma.ai
