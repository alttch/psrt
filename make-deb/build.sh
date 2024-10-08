#!/bin/bash -xe

PSRT=..

VERSION=$(grep ^version ${PSRT}/Cargo.toml|awk '{ print $3 }'|tr -d '"')

[ -z "$VERSION" ] && exit 1

if [ "$1" = "enterprise" ]; then
  PACKAGE="psrt-enterprise"
else
  PACKAGE="psrt"
fi

TARGET="${PACKAGE}-${VERSION}-amd64${PACKAGE_SUFFIX}"
[ -z "${RUST_TARGET}" ] && RUST_TARGET=x86_64-unknown-linux-musl
[ -z "${TARGET_DIR}" ] && TARGET_DIR=target

rm -rf "./${TARGET}"
mkdir -p "./${TARGET}/usr/bin"
mkdir -p "./${TARGET}/usr/sbin"
mkdir -p "./${TARGET}/lib/systemd/system"
mkdir -p "./${TARGET}/DEBIAN"
cp -vf "${PSRT}/${TARGET_DIR}/${RUST_TARGET}/release/psrt-cli" "./${TARGET}/usr/bin/"
cp -vf "${PSRT}/${TARGET_DIR}/${RUST_TARGET}/release/psrtd" "./${TARGET}/usr/sbin/"
cp -vf "${PSRT}/psrtd.service" "./${TARGET}/lib/systemd/system/"
cp -rvf ./etc "./${TARGET}/"
strip "./${TARGET}/usr/bin/psrt-cli"
strip "./${TARGET}/usr/sbin/psrtd"
(
cat << EOF
Package: ${PACKAGE}
Version: ${VERSION}
Section: base
Priority: optional
Architecture: amd64
Maintainer: Serhij S. <div@altertech.com>
Description: Industrial Pub-Sub server with minimal latency and MQTT-compatible logic
EOF
) > "./${TARGET}/DEBIAN/control"
cp -vf ./debian/* "./${TARGET}/DEBIAN/"
dpkg-deb --build --root-owner-group -Zxz "./${TARGET}"
