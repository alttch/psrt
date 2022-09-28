# PSRT - industrial Pub/Sub for WAN

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

Topic subscriptions in PSRT are processed with B-tree algorithms, which allows
the server to handle hundred thousands subscriptions without any speed
loss.

## Technical documentation

<https://info.bma.ai/en/actual/psrt/>

## About the authors

[Bohemia Automation](https://www.bohemia-automation.com) /
[Altertech](https://www.altertech.com) is a group of companies with 15+ years
of experience in the enterprise automation and industrial IoT. Our setups
include power plants, factories and urban infrastructure. Largest of them have
1M+ sensors and controlled devices and the bar raises higher and higher every
day.
