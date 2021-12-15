# Pub/Sub Realtime Telemetry Protocol (PSRT) Specifications

## Common

Default port: 2873

All numbers are processed as little-endian.

PROTOCOL VERSION: 1

LEN = data length (4 bytes = u32)

Response codes:

* 0x00 - NOP (pings, should be sent every N < timeout seconds to make sure the
  socket is alive)
* 0x01 - OK
* 0x02 - OK\_WAITING (waiting for data, used in replication only)
* 0xE0 - NOT\_REQUIRED (used in replication only)
* 0xFE - ERR\_ACCESS (pub/sub access denied)
* 0xFF - ERROR (all other errors)

Data sockets usually use the same port as control ones.

Priority byte is not used at the moment (reserved for future) and should be
always 0x7F.

## Control socket

### Greetings

Client: EE AA <00 / 01 for STARTTLS>

Server: sets mode or disconnects

Server (4 byte): EE AA 01 00 (01 00 - protocol version)

Client: LEN LOGIN 00 PASSWORD (for anonymous login send = 01 00 00 00 00)

Server: 32 bytes (client token) or closes the socket

### Keep-alive pings

The client MUST ping the server by sending OP\_NOP with frequency higher than
the server timeout, otherwise the socket is automatically closed by the server.

Any other control frame can be used as keep-alive signal as well. So if the
client sends many control frames, ping frames can be delayed or omitted.

### Subscribe

OP\_SUBCRIBE = 0x02

Client: 

0x02 LEN TOPIC (or multiple topics split with 0x00)

Server:

OK/ERROR

### Unsubscribe

OP\_UNSUBCRIBE = 0x03

Client:

0x03 LEN TOPIC (or multiple topics split with 0x00)

Server:

OK/ERROR

### Publish

OP\_PUBLISH = 0x01

Client:

0x01 PRI(u8=7F) LEN TOPIC \x0 MESSAGE

Server:

OK/ERROR

### Publish if subscribed (replicate)

OP\_PUBLISH\_REPL = 0x11

Client:

0x11 LEN TOPIC

Server:

OK\_WAIT/ERROR/NOT\_REQIRED

Client (if required)

PRIO(u8=7F) TIMESTAMP(u64, nanoseconds) LEN MESSAGE

### Bye

OP\_BYE = 0xFF

Client:

0xFF

Server: closes the socket

## Data socket

Note: A data socket is forcibly disconnected by the server when the client is
disconnected from the control socket.

### Greetings

Client: 0xEE 0xAB <0x00 / 0x01 for STARTTLS>

Server: sets mode or disconnects

Server (4 byte): EE AB 01 00 (01 00 - protocol version)

Client: 32 bytes (client token) TIMEOUT\_SEC (u8)

Server: OK or closes the socket

The client can have only one data socket with the same token. The client may
close the data socket and then reuse the token to open a new one.

### Keep-alive pings

The server pings the client by sending OP\_NOP with frequency TIMEOUT\_SEC / 2,
where TIMEOUT\_SEC is the value reported by the client during greetings.

### Message push

Server:

x01 PRI(u8=7F) LEN TOPIC 00 MESSAGE

The server also sends beacon OP\_NOP messages with TIMEOUT/2 interval.

### Bye

Not required. The client can close the data socket at any time.

## UDP

* 0x01 - OP\_PUBLISH
* 0x21 - OP\_PUBLISH\_NO\_ACK

### Plain

Client:

EE AA 01 00 (VERSION) 00 (PLAIN) LOGIN 00 PASSWORD 00 OP PRIO(u8=7F) TOPIC 00 DATA

Server (if ack required):

CONTROL\_HEADER (2 bytes) PROTOCOL\_VER (2 bytes) OK

### Encrypted

Client:

EE AA 01 00 (VERSION) ENC\_TYPE LOGIN 00 NONCE(12 bytes) ENC\_BLOCK DIGEST

where ENC\_TYPE:

* 0x02 - AES128-GCM
* 0x03 - AES256-GCM

and ENC\_BLOCK: OP PRIO(u8=7F) TOPIC 00 DATA

Server (if ack required):

CONTROL\_HEADER (2 bytes) PROTOCOL\_VER (2 bytes) OK
