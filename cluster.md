# PubSubRT clusters

* Clusters are supported by PSRT enterprise version only.

* If you do not have valid PSRT enterprise license, please contact
  [Altertech](https://www.altertech.com/order/) for quotes and contract
  details.

## Cluster nodes

The cluster nodes have to be placed as near as possible, have low network
latency between and enough channel bandwidth.

### Node configuration

* To replicate incoming messages each node must be configured individually,
having list of neighbors configured in the cluster configuration YAML file.

* Path to the cluster configuration file must be set in "cluster/config"
  field of the main configuration file (usually "config.yml").

* The configuration file contains a list of neighbors, which must have the
  following format:

```yaml
- path: 127.0.0.1:2883
  user: repl
  password: "123"
  timeout: 5
- path: 127.0.0.1:4883
  user: repl
  password: "password"
  #tls: true
  #tls_ca: /path/to/certs/ca.crt
```

* Neighbor nodes must have replication user set up. The account should have
  "replicator: true" option in its ACL to enable replication operations. No
  additional ACL options are required: it is allowed to replicator users to
  replicate all topics.

* To enable clustering, license keys must be uploaded to all cluster nodes. A
  license key is individual for each node in cluster.

* Paths to license key files must be set in "server/license" field of the main
  configuration file.

### Usage

* After booting, all cluster nodes enter into "waiting" state. As soon as a node
receives first message, it tries to connect all cluster neighbors and submit it
to them. If succeed, the neighbor nodes become "online" in the cluster status
table.

* Clients can publish / subscribe to any node in the cluster and exchange
messages the same way as using a standalone server.

* Node replication status is monitored on the main status web page,
  individually for the each node in the cluster.
