VERSION=0.1.1

all: test

clean:
	rm -rf _build
	cargo clean

tag:
	git tag -a v${VERSION} -m v${VERSION}
	git push origin --tags

ver:
	sed -i 's/^version = ".*/version = "${VERSION}"/g' Cargo.toml

release: pub tag pkg debian-pkg pub-pkg

pub: publish-cargo-crate

publish-cargo-crate:
	cargo publish

pkg:
	rm -rf _build
	mkdir -p _build
	cross build --target x86_64-unknown-linux-musl --release --features server,cli
	cross build --target i686-unknown-linux-musl --release --features server,cli
	cross build --target arm-unknown-linux-musleabihf --release --features server,cli
	cross build --target aarch64-unknown-linux-musl --release --features server,cli
	cd target/x86_64-unknown-linux-musl/release && tar czvf ../../../_build/psrt-${VERSION}-x86_64-musl.tar.gz psrtd psrt-cli
	cd target/i686-unknown-linux-musl/release && tar czvf ../../../_build/psrt-${VERSION}-i686-musl.tar.gz psrtd psrt-cli
	cd target/arm-unknown-linux-musleabihf/release && tar czvf ../../../_build/psrt-${VERSION}-arm-musleabihf.tar.gz psrtd psrt-cli
	cd target/aarch64-unknown-linux-musl/release && \
			aarch64-linux-gnu-strip psrtd && \
			aarch64-linux-gnu-strip psrt-cli && \
			tar czvf ../../../_build/psrt-${VERSION}-aarch64-musl.tar.gz psrtd psrt-cli

debian-pkg:
	cd make-deb && ./build.sh && mv psrt-${VERSION}-amd64.deb ../_build/

pub-pkg:
	cd _build && echo "" | gh release create v$(VERSION) -t "v$(VERSION)" \
			psrt-${VERSION}-arm-musleabihf.tar.gz \
			psrt-${VERSION}-i686-musl.tar.gz \
			psrt-${VERSION}-x86_64-musl.tar.gz \
			psrt-${VERSION}-aarch64-musl.tar.gz \
			psrt-${VERSION}-amd64.deb

release-enterprise:
	cargo build --target x86_64-unknown-linux-musl --release --features server,cli,cluster
	cd make-deb && \
		./build.sh enterprise && \
		gsutil cp -a public-read psrt-enterprise-${VERSION}-amd64.deb gs://get.eva-ics.com/psrt-enterprise/
	jks build get.eva-ics.com
