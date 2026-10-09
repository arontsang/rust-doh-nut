# Rusty Doh Nut

A simple Rust project to tunnel DNS requests over Http/SPDY/QUIC

At present, because Cloudflare only supports Http/2.0 on 1.1.1.1, this is what this project targets.

This is a DNS stub resolver that uses a DOH server to recursively resolve DNS queries.



## Parameters

-p UDP and TCP port to serve on. Defaults to port 15353.

-s DOH POST server address to resolve. Defaults to Cloudflare.

   You can also switch to https://dns.google/dns-query

## Deadlines

Upstream DoH connections have a 3-second connection timeout. Each DoH request
has a 5-second total timeout, including reading the response body, for both UDP
and TCP queries.

TCP clients have 5 seconds to send a complete query (length prefix and body),
and responses have a separate 5-second write deadline. TCP errors and timeouts
close only the affected connection. Failed UDP lookups are logged and dropped.

## TCP Connections

TCP connections support multiple queries and pipelining, with up to 16 queries
in flight per connection. Responses are written as complete length-prefixed
frames in completion order, so clients must match answers by DNS transaction ID.
The connection stays open until the client closes it or a deadline or error
occurs. A client that closes its write side can still read outstanding replies.
The query-read deadline applies to each frame, including idle time between
queries, and starts when a slot is available in the pipeline.

## Concurrency Limits

UDP and TCP share a global limit of 128 admitted queries. A query holds its slot
through resolution and response delivery (UDP reply enqueue or TCP frame write).
UDP dispatch waits for capacity before spawning work; its bounded receive queue
and the socket buffer absorb bursts, with excess datagrams eventually dropped.
TCP queries wait up to 5 seconds for global capacity before closing the connection.

At most 128 TCP connections are handled at once; excess connections are closed
without spawning a handler. Each accepted connection retains its separate
16-query pipeline limit. Limits are currently fixed rather than CLI options.

## Performance

I've been able to get the overhead down to about 3ms on requests over plan DNS.

    aron@htpc:~$ dig reddit.com +noall +stats @172.20.3.1 -p 15354
    
    ; <<>> DiG 9.11.3-1ubuntu1.12-Ubuntu <<>> reddit.com +noall +stats @172.20.3.1 -p 15354
    ;; global options: +cmd
    ;; Query time: 8 msec
    ;; SERVER: 172.20.3.1#15354(172.20.3.1)
    ;; WHEN: Sat May 30 22:52:22 HKT 2020
    ;; MSG SIZE  rcvd: 103
    
    aron@htpc:~$ dig reddit.com +noall +stats @8.8.8.8

    ; <<>> DiG 9.11.3-1ubuntu1.12-Ubuntu <<>> reddit.com +noall +stats @8.8.8.8
    ;; global options: +cmd
    ;; Query time: 5 msec
    ;; SERVER: 8.8.8.8#53(8.8.8.8)
    ;; WHEN: Sat May 30 22:52:26 HKT 2020
    ;; MSG SIZE  rcvd: 103
