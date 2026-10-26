# Crossfir3d server

The server side of [Crossfir3d](https://github.com/prototypo/Crossfir3d), in Rust. The
plan: a secure Crossfire server we can host on AWS.

The first step is a **gateway**. Players connect to it, never to the Crossfire C
server, which runs behind it and cannot be reached from outside. Parts of the C
server move to Rust one at a time, behind the gateway.

## The gateway (`crates/gateway`)

What it does for each player connection:

- **TLS only.** The Crossfire protocol sends passwords as plain text; the gateway
  only accepts encrypted connections. (The Crossfir3d client uses TLS for any server
  that is not on the same computer.)
- **Known commands only.** Every client message must be one of the 22 commands the
  C server takes (`socket/loop.cpp`). Anything else closes the connection.
- **Size limits.** A message is 1 to 4096 bytes.
- **No `dm` from the network.** `dm` and `dmhide` (and their short forms) close the
  connection, unless `GATEWAY_ALLOW_DM=1`. A DM logs in on the server machine.
- **Password tries.** 10 tries per address per 10 minutes, across reconnects (the C
  server only counts 5 per connection).
- **Rate limits.** 600 messages at once, then 60 a second, per connection; 4
  connections per address (an IPv6 /64 counts as one address); 256 in all.
- **Timeouts.** TLS and the first message within 10 s; 10 minutes with no message
  closes the connection. The C server is only contacted after a valid first message.

The server's answers are passed back unchanged.

## Run it

```sh
cargo build --release
GATEWAY_CERT=cert.pem GATEWAY_KEY=key.pem ./target/release/gateway
```

| Setting | Default | What |
| --- | --- | --- |
| `GATEWAY_LISTEN` | `0.0.0.0:13328` | Where players connect |
| `GATEWAY_SERVER` | `127.0.0.1:13327` | The Crossfire C server |
| `GATEWAY_CERT`, `GATEWAY_KEY` | (required) | PEM certificate chain and private key |
| `GATEWAY_ALLOW_DM` | off | `1` lets `dm` through |

To try it on one computer: make a certificate for the computer's `.local` name (not
`localhost`, which the client reaches without TLS), run the C server and the gateway,
and start the client with `CROSSFIR3D_CA=cert.pem`, connecting to `name.local:13328`.

## Tests

```sh
cargo test
```

## Licence

GPL-2.0-or-later, like Crossfire.
