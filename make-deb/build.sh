#!/bin/bash -xe

PSRT=..

VERSION=$(grep ^version ${PSRT}/Cargo.toml|awk '{ print $3 }'|tr -d '"')

[ -z "$VERSION" ] && exit 1

TARGET="psrt-${VERSION}"

rm -rf "./${TARGET}"
mkdir -p ./${TARGET}/usr/bin
mkdir -p ./${TARGET}/usr/sbin
mkdir -p ./${TARGET}/lib/systemd/system
mkdir -p ./${TARGET}/DEBIAN
cp -vf ${PSRT}/target/x86_64-unknown-linux-musl/release/psrt-cli ./${TARGET}/usr/bin/
cp -vf ${PSRT}/target/x86_64-unknown-linux-musl/release/psrtd ./${TARGET}/usr/sbin/
cp -vf ${PSRT}/psrtd.service ./${TARGET}/lib/systemd/system/
cp -rvf ./etc ./${TARGET}/
strip ./${TARGET}/usr/bin/psrt-cli
strip ./${TARGET}/usr/sbin/psrtd
# TODO configs
# TODO systemd service
(
cat << EOF
Package: psrt
Version: ${VERSION}
Section: base
Priority: optional
Architecture: amd64
Maintainer: Sergei S. <div@altertech.com>
Description: Industrial Pub-Sub server with minimal latency and MQTT-compatible logic
EOF
) > ./${TARGET}/DEBIAN/control
cp -vf ./debian/* ./${TARGET}/DEBIAN/
dpkg-deb --build ./${TARGET}
