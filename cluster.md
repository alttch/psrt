# PubSubRT clusters

* Clusters are supported with PubSubRT Enterprise version only.

* If you do not have valid PubSubRT Enterprise license, please contact
  [Altertech](https://www.altertech.com/order/?id=psrt) for quotes and contract
  details.

## Cluster nodes

The cluster nodes have to be placed as near as possible to each other, have low
network latency between and enough channel bandwidth.

Clustering raises message latencies, produces additional overhead in data
processing and is used only for the high availability of pub/sub services. It
is recommended to keep node amount in a single cluster low (2-3), unless higher
HA standards are required.

### Node configuration

* To replicate incoming messages each node must be configured individually,
having list of neighbors configured in the cluster configuration YAML file.

* Path to the cluster configuration file must be set in "cluster/config"
  field of the main configuration file (usually "config.yml").

* The configuration file contains a list of neighbors, which must have the
  following format:

```yaml
- path: node2:2873
  user: repl
  password: "123"
  timeout: 5
- path: node3:2873
  user: repl
  password: "password"
  #tls: true
  #tls_ca: /path/to/certs/ca.crt
```

* Neighbor nodes must have replication user set up. The account must have
  "replicator: true" option in its ACL to enable replication operations. No
  additional ACL options are required: it is allowed to replicator users to
  replicate all topics.

* To enable clustering, license keys must be uploaded to all cluster nodes. A
  license key is individual for the each node in cluster.

* Paths to license key files must be set in "server/license" field of the main
  configuration file.

## Usage

* After booting, all cluster nodes enter into "waiting" state. As soon as a node
receives first message, it tries to connect all cluster neighbors and submit it
to them. If succeed, the neighbor nodes become "online" in the cluster status
table.

* Clients can publish / subscribe to any node in the cluster and exchange
messages the same way as using a standalone server.

* Node replication status is monitored on the main status web page,
  individually for the each node in the cluster.
