# Rusty Doh Nut

A simple Rust project to tunnel DNS requests over Http/SPDY/QUIC

At present, because Cloudflare only supports Http/2.0 on 1.1.1.1, this is what this project targets.

This is a DNS stub resolver that uses a DOH server to recursively resolve DNS queries.



## Parameters

-p UDP Port to serve on. Defaults to port 15353.

-s DOH POST server address to resolve. Defaults to Cloudflare.

   You can also switch to https://dns.google/dns-query

## Deadlines

Upstream DoH connections have a 3-second connection timeout. Each DoH request
has a 5-second total timeout, including reading the response body, for both UDP
and TCP queries.

TCP clients have 5 seconds to send a complete query (length prefix and body),
and responses have a separate 5-second write deadline. TCP errors and timeouts
close only the affected connection. Failed UDP lookups are logged and dropped.

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
