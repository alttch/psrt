#!/bin/sh

CMD=$1

shift

case ${CMD} in
  server)
    cargo run --release --bin psrtd --features server -- --config ./test-configs/config.yml $*
    ;;
  cserver)
    cargo run --release --bin psrtd --features server,cluster -- --config ./test-configs/config.yml $*
    ;;
  benchmark)
    cargo run --release --bin psrt-cli --features cli -- localhost:2873 --benchmark $*
    ;;
  top)
    cargo run --release --bin psrt-cli --features cli -- localhost:2873 -t '#' --top $*
    ;;
  *)
    echo "command unknown: ${CMD}"
    ;;
esac
