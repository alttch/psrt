# PSRT protocol specifications

## Common

Default port: 2883

All numbers are processed as little-endian.

PROTOCOL VERSION: 1

LEN = packet length (4 bytes = u32)

Response frames:

* 0x00 - NOP (pings, should be sent every N < timeout seconds to make sure the
  socket is alive)
* 0x01 - OK
* 0x02 - OK\_WAITING (waiting for data, used in replication only)
* 0xE0 - NOT\_REQUIRED (used in replication only)
* 0xFE - ERR\_ACCESS (pub/sub access denied)
* 0xFF - ERROR (all other errors)

Data sockets usually use the same port as control ones.

Priority byte is not used at the moment (reserved for the future) and should be
always 0x7F.

## Control socket

### Greetings

Client: EE AA <00 / 01 for STARTTLS>

Server: sets mode or disconnects

Server (4 byte): EE AA 01 00 (01 00 - protocol version)

Client: LEN LOGIN 00 PASSWORD (for anonymous login send = 01 00 00 00 00)

Server: 32 bytes (*client token) or closes the socket

### Subscribe

OP\_SUBCRIBE = 0x02

Client: 

0x02 LEN TOPIC (or multiple topics splitted by 0x00)

Server:

OK/ERROR

### Unsubscribe

OP\_UNSUBCRIBE = 0x02

Client:

0x03 LEN TOPIC (or multiple topics splitted by 0x00)

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

Server: closes socket

## Data socket

### Greetings

Client: 0xEE 0xAB <0x00 / 0x01 for STARTTLS>

Server: sets mode or disconnects

Server (4 byte): EE AB 01 00 (01 00 - protocol version)

Client: 32 bytes (hash) TIMEOUT\_SEC (u8)

Server: OK or closes the socket

### Message push

Server:

x01 PRI(u8=7F) LEN TOPIC 00 MESSAGE

Server will also send beacon messages with TIMEOUT/2 interval

Data socket MUST be forcibly disconnected by the server when the client is
disconnected from the control socket.

## UDP

* 0x01 - OP\_PUBLISH
* 0x21 - OP\_PUBLISH\_NO\_ACK

Client:

EE AA 01 00 LOGIN 00 PASSWORD 00 OP PRIO(u8=7F) TOPIC 00 DATA

Server (if ack required):

CONTROL\_HEADER (2 bytes) PROTOCOL\_VER (2 bytes) OK
