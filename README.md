# PubSubRT - an industrial real-time pub/sub server

<img src="https://raw.githubusercontent.com/alttch/psrt/main/psrt-logo.png"
width="100" />

## What is PSRT

PSRT is a real-time pub-sub protocol, optimized for industrial needs: providing
low latency, dealing slow channels and large payloads.

PSRT can process 100K+ messages on a single node with very low latencies
(<1ms). Speeds are reasonable even with large (1M+ payloads).

## Why not MQTT?

We love MQTT. And we use MQTT a lot. There are cases where MQTT ideally fits
requirements. However, for some it does not satisfy our speed and reliability
needs and produces additional overhead. That is why we invented PSRT.

## What is the difference?

* PSRT is the protocol, optimized for large (65K+) messages
* No QoS - all messages are always delivered to subscribers only once, so
  consider it is always QoS=2 if using MQTT measurements
* No retain topics. Retains usually require disk writes, which produce
  additional overhead
* No OP-ACK loops. All control operations are fast, synchronous and atomic
* Two TCP sockets: one for control ops and the second one for incoming
  messages. This makes clients a bit more complicated, but allows to process
  incoming messages without additional overhead. Additionally, with two sockets
  control op acknowledgements and incoming message data can be mixed, which is
  important when large messages are processed on slow channels
* Devices and nodes, which do not need subscriptions, can use either a single
  TCP control socket or work without any connection established, using UDP
  datagrams with or without acknowledge from the server
* PSRT is almost 100% logically compatible with MQTT, so software can be
  switched to it and vice versa with only a couple of lines of code

## About the repository

This repository contains PubSubRT server, implementing PSRT (open-source
version without the cluster module), command-line client and Rust client
library.

Other repositories:

* [psrt-py](https://github.com/alttch/psrt-py) - Python client library (sync),
  SDK is semi-compatible with
  [paho-mqtt](https://github.com/eclipse/paho.mqtt.python)

## Installation

Use binaries from <https://github.com/alttch/psrt/releases>. For Debian/Ubuntu
and other deb-based distros, *.deb packages can be used.

## Build from source

* Install [Rust](https://www.rust-lang.org/tools/install)
* Build the server and cli:

```shell
cargo build --features server,cli --release
```

## Example configuration files

If installed manually, get configuration files from
<https://github.com/alttch/psrt/tree/main/make-deb/etc/psrtd> or use files from
*example-configs* directory (need to be edited before use).

## Authentication

PubSubRT uses the standard *htpasswd* format, use any htpasswd-compatible tool
(with -B flag for bcrypt).

## Statistical data

Users with admin rights can obtain statistical data using a web browser (by
default, at http://localhost:2884).

The data can also be obtained in JSON for 3rd-party apps at:

```shell
curl http://localhost:2884/status
```

If the anonymous user has no admin rights, URI requires login and password
(HTTP basic auth).

## Cargo crate

<https://crates.io/crates/psrt>

## Protocol specifications

<https://github.com/alttch/psrt/blob/main/proto.md>

## Enterprise version

Download packages from <https://get.eva-ics.com/psrt-enterprise/>
