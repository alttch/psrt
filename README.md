# PubSubRT - an industrial real-time pub/sub server

<img src="https://raw.githubusercontent.com/alttch/psrt/main/psrt-logo.png"
width="100" />

## What is PSRT

PSRT is a pub/sub real-time telemetry protocol, optimized for industrial needs:
providing low latency, dealing with slow channels and large payloads.

PSRT can process 100K+ messages on a single node with very low latencies
(<1ms). Speeds are reasonable (1K+ ops/sec) even with enormous (1MB+) payloads.

PSRT is designed to work in wide-area networks, where connections can be slow,
unstable and clients must be authenticated. If you need a local-network
or local-host solution with even lower latencies, higher speeds, more features,
but zero security, try [ELBUS](https://github.com/alttch/elbus/).

Topic subscriptions in PubSubRT are processed with B-tree algorithms, which
allows the server to handle hundred thousands subscriptions without any speed
loss.

## Why not MQTT?

We love MQTT. And we use MQTT a lot. There are cases where MQTT ideally fits
requirements. However, for some it does not satisfy our speed and reliability
needs and produces additional overhead. That is why we invented PSRT and use it
as the primary protocol for [EVA ICS](https://www.eva-ics.com) in large
enterprise setups.

## What is the difference?

* PSRT is the protocol, optimized for large (65K+) message payloads
* No QoS - all messages are always delivered to subscribers only once, so
  consider it is always QoS=2 if use MQTT measurements
* No retain topics. Retains usually require disk writes, which produce
  additional overhead
* No OP-ACK loops. All control operations are fast, synchronous and atomic
* Two TCP sockets: one for control ops and the second one for incoming
  messages. This makes clients a bit more complicated, but allows to process
  incoming messages without any extra overhead. Additionally, with two sockets
  control op acknowledgements and incoming message data can be mixed, which is
  important when large messages are processed on slow channels
* Devices and nodes, which do not need subscriptions, can use either a single
  TCP control socket or work without any connection established, using UDP
  datagrams with or without acknowledge from the server
* PSRT is almost 100% logically compatible with MQTT, so software can be
  switched to it and vice versa with only a couple of lines of code

## What is the same?

PSRT is logically the same as MQTT: same format for topics, same format for
topic masks etc:

* path/to/topic - an individual topic (subscribe / publish)
* path/to/# - all topics under the specified path (subscribe)
* path/+/some/+/topic - all topics matching the mask ("+" for any subtopic)

## About the repository

This repository contains PubSubRT server, implementing PSRT (open-source
version without the cluster module), command-line client and Rust client
library. The cluster code is not open-source, but may be opened in the future
as well.

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

## Configuration files

If installed manually, get configuration files from
<https://github.com/alttch/psrt/tree/main/make-deb/etc/psrtd> or use files from
*test-configs* directory (need to be edited before use).

If installed from deb-package, configuration files are stored in */etc/psrtd*
directory.

The official port numbers, assigned by IANA for PSRT are 2873 TCP/UDP. It is
recommended to keep these ports on servers to let clients connect / push data
using defaults.

## Problems

* If any problems occur, try running **psrtd** with *-v* argument to get
  verbose logging in terminal.

* The most common problem is timeout disconnect. The server timeout MUST be
  higher than the slowest expected client timeout.

## Authentication

PubSubRT uses the standard *htpasswd* format, use any htpasswd-compatible tool
(with -B flag for bcrypt).

Passwords file and ACL can be reloaded on-the-flow. Use either *kill -HUP
$SERVER_PID* or *systemctl reload psrtd* (if systemd service is configured).

## Statistical data

### Overview (web interface / API)

Users with admin rights can obtain statistical data using a web browser (by
default, at http://localhost:8880).

<img
src="https://raw.githubusercontent.com/alttch/psrt/main/screenshots/web_status.png"
width="750" />

The data can also be obtained in JSON for 3rd-party apps at:

```shell
curl http://localhost:8880/status
```

If the anonymous user has no admin rights, URI requires login and password
(HTTP basic auth).

### Most used topics

By executing **psrt-cli** with *--top* argument, the most used topics can be
monitored in console in real-time. Use "s" key to switch sorting between
message count and bytes.

<img
src="https://raw.githubusercontent.com/alttch/psrt/main/screenshots/cli_top.png" />

## Cargo crate (async client)

<https://crates.io/crates/psrt>

## Protocol specifications

<https://github.com/alttch/psrt/blob/main/proto.md>

## UDP encryption

PSRT supports symmetrical encrypted UDP frames (see the protocol
specifications). Currently supported encryption modes: AES128-GCM and
AES256-GCM.

To enable UDP encryption, add to "auth" section of the main config:

```yaml
auth:
    # ........
    key_file: keys.yml
```

The keys file has the following format and there can be only one encryption key
per user:

```yaml
user1: <aes_key>
user2: <aes_key>
```

where aes\_key is a random 32-byte (for AES128 only first 16 bytes are used)
hex sequence, which can be generated, e.g. with:

```shell
head -c16384 /dev/urandom|sha256sum|awk '{ print $1 }'
```

## Enterprise version

Download packages from <https://pub.bma.ai/psrt-enterprise/>

[Cluster setup
instructions](https://github.com/alttch/psrt/blob/main/cluster.md)

The Enterprise version can be tested in "unlimited trial" mode. Feel free to
download testing [key
files](https://github.com/alttch/psrt/tree/main/enterprise-keys). Each key file
is bound to the specific host name, so the system host names in "unlimited
trial" PSRT Enterprise clusters must be "node1", "node2" and "node3".

## About the authors

[Bohemia Automation](https://www.bohemia-automation.com) /
[Altertech](https://www.altertech.com) is a group of companies with 15+ years
of experience in the enterprise automation and industrial IoT. Our setups
include power plants, factories and urban infrastructure. Largest of them have
1M+ sensors and controlled devices and the bar raises upper and upper every
day.
