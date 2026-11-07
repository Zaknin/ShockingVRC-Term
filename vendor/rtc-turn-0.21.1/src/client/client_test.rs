use super::*;
use sansio::Protocol;
use std::collections::HashSet;
use std::net::UdpSocket;

/// Tests may resolve the built-in provider; library code never does.
fn test_crypto_provider() -> std::sync::Arc<dyn crypto::RTCCryptoProvider> {
    crypto::default_provider().expect("a built-in crypto provider must be enabled for tests")
}

fn refresh_success_response(transaction_id: TransactionId, lifetime: Duration) -> Message {
    let mut response = Message::new();
    response
        .build(&[
            Box::new(transaction_id),
            Box::new(MessageType::new(METHOD_REFRESH, CLASS_SUCCESS_RESPONSE)),
            Box::new(crate::proto::lifetime::Lifetime(lifetime)),
        ])
        .expect("build Refresh success response");
    response
}

fn turn_error_response(method: Method, transaction_id: TransactionId, code: u16, nonce: &str) -> Message {
    let mut response = Message::new();
    response
        .build(&[
            Box::new(transaction_id),
            Box::new(MessageType::new(method, CLASS_ERROR_RESPONSE)),
            Box::new(ErrorCodeAttribute {
                code: stun::error_code::ErrorCode(code),
                reason: format!("test error {code}").into_bytes(),
            }),
            Box::new(Nonce::new(ATTR_NONCE, nonce.to_owned())),
        ])
        .expect("build TURN error response");
    response
}

fn create_listening_test_client(rto_in_ms: u64) -> Result<(UdpSocket, Client)> {
    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;

    let client = Client::new(
        ClientConfig {
            stun_serv_addr: String::new(),
            turn_serv_addr: String::new(),
            local_addr: udp_socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: String::new(),
            password: String::new(),
            realm: String::new(),
            software: "TEST SOFTWARE".to_owned(),
            rto_in_ms,
            allocation_refresh_interval_cap: None,
        },
        test_crypto_provider(),
    )?;

    Ok((udp_socket, client))
}

fn create_listening_test_client_with_stun_serv() -> Result<(UdpSocket, Client)> {
    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;

    let client = Client::new(
        ClientConfig {
            stun_serv_addr: "stun1.l.google.com:19302".to_owned(),
            turn_serv_addr: String::new(),
            local_addr: udp_socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: String::new(),
            password: String::new(),
            realm: String::new(),
            software: "TEST SOFTWARE".to_owned(),
            rto_in_ms: 0,
            allocation_refresh_interval_cap: None,
        },
        test_crypto_provider(),
    )?;

    Ok((udp_socket, client))
}

/// A host demultiplexing STUN on a socket it shares — a STUN gatherer and this client pointed at
/// one server address — routes a response by transaction id, because nothing else identifies its
/// owner. See webrtc-rs/webrtc#890.
#[test]
fn test_has_transaction_reports_outstanding_requests() -> Result<()> {
    let (_conn, mut client) = create_listening_test_client(0)?;
    let server = "127.0.0.1:3478".parse::<std::net::SocketAddr>().unwrap();

    let unknown = TransactionId::new();
    assert!(
        !client.has_transaction(&unknown),
        "a fresh client is waiting on nothing"
    );

    let tid = client.send_binding_request_to(Instant::now(), server)?;
    assert!(
        client.has_transaction(&tid),
        "the transaction just started is outstanding"
    );
    assert!(
        !client.has_transaction(&unknown),
        "an id this client never sent is not its own"
    );

    Ok(())
}

#[test]
fn test_client_with_stun_send_binding_request() -> Result<()> {
    //env_logger::init();

    let (conn, mut client) = create_listening_test_client_with_stun_serv()?;
    let local_addr = conn.local_addr()?;

    let tid = client.send_binding_request(Instant::now())?;

    while let Some(transmit) = client.poll_write() {
        conn.send_to(&transmit.message, transmit.transport.peer_addr)?;
    }

    let mut buffer = vec![0u8; 2048];
    let (n, peer_addr) = conn.recv_from(&mut buffer)?;
    client.handle_read(TransportMessage {
        now: Instant::now(),
        transport: TransportContext {
            local_addr,
            peer_addr,
            transport_protocol: TransportProtocol::UDP,
            ecn: None,
        },
        message: BytesMut::from(&buffer[..n]),
    })?;

    if let Some(event) = client.poll_event() {
        match event {
            Event::BindingResponse(id, refl_addr) => {
                assert_eq!(tid, id);
                debug!("mapped-addr: {}", refl_addr);
            }
            _ => assert!(false),
        }
    } else {
        assert!(false);
    }

    assert_eq!(0, client.tr_map.size(), "should be no transaction left");

    client.close()
}

#[test]
fn test_client_with_stun_send_binding_request_to_parallel() -> Result<()> {
    //env_logger::init();

    let (conn, mut client) = create_listening_test_client(0)?;
    let local_addr = conn.local_addr()?;

    let to = lookup_host(true, "stun1.l.google.com:19302")?;

    let tid1 = client.send_binding_request_to(Instant::now(), to)?;
    let tid2 = client.send_binding_request_to(Instant::now(), to)?;
    while let Some(transmit) = client.poll_write() {
        conn.send_to(&transmit.message, transmit.transport.peer_addr)?;
    }

    let mut buffer = vec![0u8; 2048];
    for _ in 0..2 {
        let (n, peer_addr) = conn.recv_from(&mut buffer)?;
        client.handle_read(TransportMessage {
            now: Instant::now(),
            transport: TransportContext {
                local_addr,
                peer_addr,
                transport_protocol: TransportProtocol::UDP,
                ecn: None,
            },
            message: BytesMut::from(&buffer[..n]),
        })?;
    }

    let mut tids = HashSet::new();
    while let Some(event) = client.poll_event() {
        match event {
            Event::BindingResponse(tid, refl_addr) => {
                tids.insert(tid);
                debug!("mapped-addr: {}", refl_addr);
            }
            _ => {}
        }
    }

    assert_eq!(2, tids.len());
    assert!(tids.contains(&tid1));
    assert!(tids.contains(&tid2));

    client.close()
}

#[test]
fn test_client_with_stun_send_binding_request_to_timeout() -> Result<()> {
    //env_logger::init();

    let (conn, mut client) = create_listening_test_client(10)?;

    let to = lookup_host(true, "127.0.0.1:9")?;

    let tid = client.send_binding_request_to(Instant::now(), to)?;
    while let Some(transmit) = client.poll_write() {
        conn.send_to(&transmit.message, transmit.transport.peer_addr)?;
    }

    while let Some(to) = client.poll_timeout() {
        client.handle_timeout(to)?;
    }

    if let Some(event) = client.poll_event() {
        match event {
            Event::TransactionTimeout(id) => {
                assert_eq!(tid, id);
            }
            _ => assert!(false),
        }
    } else {
        assert!(false);
    }

    client.close()
}

/// The allocation-refresh timer is seeded from the instant passed to `RelayState::new` and
/// compared against the instant passed to `handle_timeout`, so a refresh can be observed by
/// arithmetic on a base instant with no wall-clock time passing and no sleeping.
#[test]
fn test_relay_refresh_timers_run_on_injected_time() -> Result<()> {
    let base = Instant::now();
    let t = |secs| base + Duration::from_secs(secs);

    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            stun_serv_addr: String::new(),
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: udp_socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "user".to_owned(),
            password: "pass".to_owned(),
            realm: "realm".to_owned(),
            software: "TEST SOFTWARE".to_owned(),
            rto_in_ms: 0,
            allocation_refresh_interval_cap: None,
        },
        test_crypto_provider(),
    )?;

    // Stand in for a granted allocation, seeded at t(0) with a 10-minute lifetime.
    let lifetime = Duration::from_secs(600);
    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "nonce".to_owned()),
            lifetime,
            None,
        ),
    );

    // The allocation is refreshed at half its lifetime; permissions sooner, at 120s.
    assert_eq!(client.relay(relayed_addr)?.poll_timeout(), Some(t(120)));

    // One second before the allocation deadline nothing is sent for it. (The permission
    // refresh at t(120) has already fired by then, so drain whatever it queued first.)
    client.relay(relayed_addr)?.handle_timeout(t(120));
    while client.poll_write().is_some() {}
    client.relay(relayed_addr)?.handle_timeout(t(299));
    assert!(
        client.poll_write().is_none(),
        "no refresh is due one second before the deadline"
    );

    // At the deadline the Refresh request goes out, stamped with the caller's instant.
    client.relay(relayed_addr)?.handle_timeout(t(300));
    let transmit = client
        .poll_write()
        .expect("the allocation must be refreshed at half its lifetime");
    assert_eq!(
        transmit.now,
        t(300),
        "the request carries the instant the caller supplied, not an ambient reading"
    );

    let mut msg = Message::new();
    msg.raw = transmit.message.to_vec();
    msg.decode()?;
    assert_eq!(msg.typ.method, METHOD_REFRESH);
    client.handle_stun_message(t(300), &refresh_success_response(msg.transaction_id, lifetime).raw)?;

    // The late permission refresh at t(299) is rescheduled from the time it was handled, so its
    // next deadline is t(419). The allocation remains due at t(600).
    assert_eq!(client.relay(relayed_addr)?.poll_timeout(), Some(t(419)));

    client.relay(relayed_addr)?.handle_timeout(t(600));
    let transmit = client
        .poll_write()
        .expect("the allocation must be refreshed again a half-lifetime later");
    assert_eq!(transmit.now, t(600));

    client.close()
}

/// Applications may refresh an otherwise idle allocation more frequently than half its server
/// lifetime so the client-to-server NAT mapping does not sit idle for several minutes.
#[test]
fn test_allocation_refresh_interval_cap_is_applied() -> Result<()> {
    let base = Instant::now();
    let t = |secs| base + Duration::from_secs(secs);

    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            stun_serv_addr: String::new(),
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: udp_socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "user".to_owned(),
            password: "pass".to_owned(),
            realm: "realm".to_owned(),
            software: "TEST SOFTWARE".to_owned(),
            rto_in_ms: 0,
            allocation_refresh_interval_cap: Some(Duration::from_secs(30)),
        },
        test_crypto_provider(),
    )?;

    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    let mut response = Message::new();
    response.build(&[
        Box::new(TransactionId::new()),
        Box::new(MessageType::new(METHOD_ALLOCATE, CLASS_SUCCESS_RESPONSE)),
        Box::new(RelayedAddress {
            ip: relayed_addr.ip(),
            port: relayed_addr.port(),
        }),
        Box::new(crate::proto::lifetime::Lifetime(Duration::from_secs(600))),
    ])?;
    client.handle_allocate_response(
        base,
        response,
        TransactionType::AllocateRequest(Nonce::new(ATTR_NONCE, "nonce".to_owned())),
    )?;

    for expected_secs in [30, 60, 90, 120, 150, 180] {
        assert_eq!(
            client.relay(relayed_addr)?.poll_timeout(),
            Some(t(expected_secs))
        );
        client.relay(relayed_addr)?.handle_timeout(t(expected_secs));

        let transmit = client
            .poll_write()
            .expect("the configured cadence must emit an allocation Refresh");
        let mut message = Message::new();
        message.raw = transmit.message.to_vec();
        message.decode()?;
        assert_eq!(message.typ.method, METHOD_REFRESH);
        client.handle_stun_message(
            t(expected_secs),
            &refresh_success_response(message.transaction_id, Duration::from_secs(600)).raw,
        )?;
    }

    assert_eq!(client.relay(relayed_addr)?.poll_timeout(), Some(t(210)));

    client.close()
}

/// Handling an overdue relay timer once must put every refreshed deadline back in the future.
/// A runtime may be suspended for longer than several refresh intervals on a mobile platform;
/// replaying each missed interval makes a Sans-I/O driver spin through historical deadlines.
#[test]
fn test_overdue_relay_refreshes_are_rescheduled_from_now() -> Result<()> {
    let base = Instant::now();
    let t = |secs| base + Duration::from_secs(secs);

    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            stun_serv_addr: String::new(),
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: udp_socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "user".to_owned(),
            password: "pass".to_owned(),
            realm: "realm".to_owned(),
            software: "TEST SOFTWARE".to_owned(),
            rto_in_ms: 0,
            allocation_refresh_interval_cap: None,
        },
        test_crypto_provider(),
    )?;

    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "nonce".to_owned()),
            Duration::from_secs(600),
            None,
        ),
    );

    let resumed_at = t(1000);
    client.relay(relayed_addr)?.handle_timeout(resumed_at);

    assert_eq!(
        client.relay(relayed_addr)?.poll_timeout(),
        Some(t(1120)),
        "one timeout pass must move the permission and allocation deadlines past now"
    );

    client.close()
}

/// A relay whose lifetime has gone to zero must not hand the caller a deadline that never
/// advances.
///
/// This is the root cause behind [webrtc#862](https://github.com/webrtc-rs/webrtc/issues/862).
/// `Relay::handle_timeout` advances the allocation timer by adding to the timer's **own**
/// previous value rather than to `now`:
///
/// ```ignore
/// relay.refresh_alloc_timer = relay.refresh_alloc_timer.add(relay.lifetime / 2);
/// ```
///
/// so the step size is `lifetime / 2`. `lifetime` is assigned straight from the server's
/// LIFETIME attribute on every refresh response, with no floor — and `Relay::close` refreshes
/// with `LIFETIME=0`, which is how RFC 5766 deallocates. Once the server echoes that back the
/// step becomes zero, the timer is frozen, and because nothing ever removes the entry from
/// `client.relays` the relay keeps reporting it forever.
///
/// The peer-connection driver treats an expired deadline as "handle it and loop again", so a
/// frozen expired deadline is an unbounded hot loop — starving the very `Close` that triggered
/// it. That matches the report: cleanup never completes and the process will not exit.
#[test]
fn test_zero_lifetime_relay_does_not_freeze_its_deadline() -> Result<()> {
    let base = Instant::now();
    let t = |secs| base + Duration::from_secs(secs);

    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            stun_serv_addr: String::new(),
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: udp_socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "user".to_owned(),
            password: "pass".to_owned(),
            realm: "realm".to_owned(),
            software: "TEST SOFTWARE".to_owned(),
            rto_in_ms: 0,
            allocation_refresh_interval_cap: None,
        },
        test_crypto_provider(),
    )?;

    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "nonce".to_owned()),
            Duration::from_secs(600),
            None,
        ),
    );

    // What the server's response to `close()`'s LIFETIME=0 refresh does.
    client.relays.get_mut(&relayed_addr).unwrap().lifetime = Duration::from_secs(0);

    // Drive past both timers, exactly as the driver does: read the deadline, handle it,
    // read again. The deadline must move, or the caller is told to act on the same instant
    // forever.
    let mut previous = client.relay(relayed_addr)?.poll_timeout();
    for iteration in 0..10 {
        let now = t(1000 + iteration);
        client.relay(relayed_addr)?.handle_timeout(now);
        let next = client.relay(relayed_addr)?.poll_timeout();

        assert!(
            next != previous || next.is_none(),
            "iteration {iteration}: deadline did not advance ({previous:?} -> {next:?}) while \
             `now` is {:?} past it. The driver reads this as a zero delay, calls handle_timeout \
             and loops — forever.",
            now.saturating_duration_since(previous.unwrap_or(now)),
        );
        previous = next;
    }

    Ok(())
}

#[test]
fn test_refresh_response_reschedules_shorter_lifetime_from_response_time() -> Result<()> {
    let base = Instant::now();
    let t = |secs| base + Duration::from_secs(secs);
    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            stun_serv_addr: String::new(),
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: udp_socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "user".to_owned(),
            password: "pass".to_owned(),
            realm: "realm".to_owned(),
            software: "TEST SOFTWARE".to_owned(),
            rto_in_ms: 0,
            allocation_refresh_interval_cap: None,
        },
        test_crypto_provider(),
    )?;
    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "nonce".to_owned()),
            Duration::from_secs(600),
            None,
        ),
    );
    assert_eq!(client.relay(relayed_addr)?.poll_timeout(), Some(t(120)));

    client
        .relay(relayed_addr)?
        .refresh_allocation(t(0), Duration::from_secs(600))?;
    let request = client.poll_write().expect("initial Refresh request");
    let mut request_message = Message::new();
    request_message.raw = request.message.to_vec();
    request_message.decode()?;
    let response = refresh_success_response(request_message.transaction_id, Duration::from_secs(60));
    client.handle_stun_message(t(30), &response.raw)?;

    assert_eq!(
        client.relay(relayed_addr)?.poll_timeout(),
        Some(t(60)),
        "a shorter server lifetime must move the allocation refresh deadline earlier"
    );
    Ok(())
}

#[test]
fn test_refresh_response_reschedule_applies_interval_cap() -> Result<()> {
    let base = Instant::now();
    let t = |secs| base + Duration::from_secs(secs);
    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            stun_serv_addr: String::new(),
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: udp_socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "user".to_owned(),
            password: "pass".to_owned(),
            realm: "realm".to_owned(),
            software: "TEST SOFTWARE".to_owned(),
            rto_in_ms: 0,
            allocation_refresh_interval_cap: Some(Duration::from_secs(20)),
        },
        test_crypto_provider(),
    )?;
    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "nonce".to_owned()),
            Duration::from_secs(600),
            Some(Duration::from_secs(20)),
        ),
    );

    client.relay(relayed_addr)?.handle_timeout(t(30));
    let request = client.poll_write().expect("scheduled Refresh request");
    let mut request_message = Message::new();
    request_message.raw = request.message.to_vec();
    request_message.decode()?;
    let response = refresh_success_response(request_message.transaction_id, Duration::from_secs(60));
    client.handle_stun_message(t(30), &response.raw)?;

    assert_eq!(client.relay(relayed_addr)?.poll_timeout(), Some(t(50)));
    Ok(())
}

/// A server confirming deallocation must leave no relay behind.
///
/// `close()` refreshes the allocation with `LIFETIME=0`; the server echoes that back to confirm
/// the allocation is gone. Before the fix for
/// [webrtc#862](https://github.com/webrtc-rs/webrtc/issues/862) the response handler stored the
/// zero and nothing else — the entry stayed in `client.relays` forever, still reporting refresh
/// deadlines for an allocation that no longer existed, and still trying to refresh it.
///
/// Flooring the refresh step stops the *hot loop*; only removing the relay stops the dead
/// allocation. Both are needed, and this covers the second.
#[test]
fn test_zero_lifetime_response_drops_the_relay() -> Result<()> {
    let base = Instant::now();
    let t = |secs| base + Duration::from_secs(secs);

    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            stun_serv_addr: String::new(),
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: udp_socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "user".to_owned(),
            password: "pass".to_owned(),
            realm: "realm".to_owned(),
            software: "TEST SOFTWARE".to_owned(),
            rto_in_ms: 0,
            allocation_refresh_interval_cap: None,
        },
        test_crypto_provider(),
    )?;

    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "nonce".to_owned()),
            Duration::from_secs(600),
            None,
        ),
    );
    assert!(client.relay(relayed_addr)?.poll_timeout().is_some());

    // What the TURN server sends back when it honours `close()`'s LIFETIME=0 refresh.
    client.relay(relayed_addr)?.close(t(0))?;
    let close = client.poll_write().expect("LIFETIME=0 close Refresh");
    let mut close_message = Message::new();
    close_message.raw = close.message.to_vec();
    close_message.decode()?;
    let response = refresh_success_response(close_message.transaction_id, Duration::ZERO);
    client.handle_stun_message(t(1), &response.raw)?;

    assert!(
        !client.relays.contains_key(&relayed_addr),
        "a deallocated allocation must not stay in the relay map — it would keep reporting \
         refresh deadlines and keep trying to refresh something that no longer exists"
    );

    Ok(())
}

/// An Allocate success response carrying `LIFETIME=0` is rejected, not accepted as a
/// degenerate allocation.
///
/// RFC 5766 §6.2 has the server take `min(client proposed, server maximum)` and fall back to
/// the *default* lifetime (600 s) whenever that does not exceed it, so a successful Allocate
/// never returns a lifetime below the default — zero is a protocol violation. Zero is
/// meaningful only on the Refresh path (§7), where it means the allocation was deleted.
///
/// Accepting it would insert a `RelayState` whose `refresh_alloc_timer` is `now.add(0)` —
/// already expired at construction — for an allocation that does not exist. That relay then
/// reports an expired refresh deadline forever, which is the hot loop in
/// [webrtc#862](https://github.com/webrtc-rs/webrtc/issues/862), reached here without any
/// close being involved.
#[test]
fn test_zero_lifetime_allocate_response_is_an_error() -> Result<()> {
    let base = Instant::now();
    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            stun_serv_addr: String::new(),
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: udp_socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "user".to_owned(),
            password: "pass".to_owned(),
            realm: "realm".to_owned(),
            software: "TEST SOFTWARE".to_owned(),
            rto_in_ms: 0,
            allocation_refresh_interval_cap: None,
        },
        test_crypto_provider(),
    )?;

    let mut res = Message::new();
    res.build(&[
        Box::new(TransactionId::new()),
        Box::new(MessageType::new(METHOD_ALLOCATE, CLASS_SUCCESS_RESPONSE)),
        Box::new(RelayedAddress {
            ip: "127.0.0.1".parse().unwrap(),
            port: 50000,
        }),
        Box::new(crate::proto::lifetime::Lifetime(Duration::from_secs(0))),
    ])?;

    client.handle_allocate_response(
        base,
        res,
        TransactionType::AllocateRequest(Nonce::new(ATTR_NONCE, "nonce".to_owned())),
    )?;

    assert!(
        client.relays.is_empty(),
        "a zero-lifetime Allocate response must not create a relay — the allocation does not \
         exist, and the relay would report an already-expired refresh deadline forever"
    );

    match client.poll_event() {
        Some(Event::AllocateError(_, _)) => Ok(()),
        other => {
            panic!("expected AllocateError for a zero-lifetime Allocate response, got {other:?}")
        }
    }
}

/// Over a TCP connection to the server the relay is still UDP: `REQUESTED-TRANSPORT` names what
/// the allocation relays, not how the client reaches the server (RFC 8656 §7.1). Deriving it from
/// the control transport asks a TCP client's server for an RFC 6062 TCP allocation instead.
#[test]
fn test_allocate_over_tcp_requests_a_udp_relay() -> Result<()> {
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: "127.0.0.1:50000".parse().unwrap(),
            transport_protocol: TransportProtocol::TCP,
            ..Default::default()
        },
        test_crypto_provider(),
    )?;
    client.allocate(Instant::now())?;

    let transmit = client.poll_write().expect("an Allocate request");
    assert_eq!(
        transmit.transport.transport_protocol,
        TransportProtocol::TCP,
        "the request itself still travels over TCP"
    );
    let mut msg = Message::new();
    msg.raw = transmit.message.to_vec();
    msg.decode()?;
    let mut requested = RequestedTransport::default();
    requested.get_from(&msg)?;
    assert_eq!(requested.protocol, PROTO_UDP, "the relay is UDP");
    Ok(())
}

/// Over a reliable transport a request is sent once and answered or timed out as a whole: TCP
/// already retransmits, and repeating the request on top of it only sends the server duplicates
/// (RFC 8489 §6.2.2). The wait is Ti, 39.5 seconds, not the UDP schedule's ~8.
#[test]
fn test_a_request_over_tcp_is_sent_once_and_times_out_at_ti() -> Result<()> {
    let start = Instant::now();
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: "127.0.0.1:50000".parse().unwrap(),
            transport_protocol: TransportProtocol::TCP,
            ..Default::default()
        },
        test_crypto_provider(),
    )?;
    client.allocate(start)?;
    // allocate() returns the id its authenticated retry will use; the timeout names the
    // request that was actually sent, so take the id from the wire.
    let sent = client.poll_write().expect("the request goes out once");
    let mut request = Message::new();
    request.raw = sent.message.to_vec();
    request.decode()?;
    let tid = request.transaction_id;

    // Walk every deadline the client asks for, up to just short of Ti.
    let ti = Duration::from_millis(39_500);
    let mut sends = 0;
    while let Some(deadline) = client.poll_timeout() {
        if deadline >= start + ti {
            break;
        }
        client.handle_timeout(deadline)?;
        while client.poll_write().is_some() {
            sends += 1;
        }
    }
    assert_eq!(sends, 0, "nothing is retransmitted over TCP");
    assert!(
        client.poll_event().is_none(),
        "no timeout before Ti has passed"
    );

    client.handle_timeout(start + ti)?;
    assert!(client.poll_write().is_none(), "not even at the deadline");
    assert!(
        matches!(client.poll_event(), Some(Event::TransactionTimeout(id)) if id == tid),
        "at Ti the request has timed out"
    );
    Ok(())
}

#[test]
fn test_refresh_retry_budget_matches_transport_timeout() {
    assert_eq!(
        timeout_budget(TransportProtocol::UDP, 200),
        Duration::from_millis(7_800),
        "UDP budget includes all seven exponentially backed-off waits"
    );
    assert_eq!(
        timeout_budget(TransportProtocol::TCP, 200),
        Duration::from_millis(39_500),
        "reliable transports use one Ti response wait"
    );
}

/// UDP keeps its schedule: the first retransmission is due one RTO after the request.
#[test]
fn test_a_request_over_udp_is_still_retransmitted() -> Result<()> {
    let start = Instant::now();
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: "127.0.0.1:50000".parse().unwrap(),
            transport_protocol: TransportProtocol::UDP,
            ..Default::default()
        },
        test_crypto_provider(),
    )?;
    client.allocate(start)?;
    assert!(client.poll_write().is_some());
    let first = client.poll_timeout().expect("a retransmit deadline");
    assert!(
        first < start + Duration::from_secs(1),
        "{:?}",
        first - start
    );
    client.handle_timeout(first)?;
    assert!(client.poll_write().is_some(), "UDP retransmits");
    Ok(())
}

#[test]
fn test_timed_out_allocation_refresh_retries_before_expiry() -> Result<()> {
    let base = Instant::now();
    let t = |secs| base + Duration::from_secs(secs);
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "original-user".to_owned(),
            password: "original-password".to_owned(),
            realm: "original-realm".to_owned(),
            rto_in_ms: 200,
            ..Default::default()
        },
        test_crypto_provider(),
    )?;
    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "original-allocation-nonce".to_owned()),
            Duration::from_secs(600),
            None,
        ),
    );

    client.relay(relayed_addr)?.handle_timeout(t(300));
    let first = client.poll_write().expect("scheduled Refresh request");
    let mut first_message = Message::new();
    first_message.raw = first.message.to_vec();
    first_message.decode()?;
    assert_eq!(first_message.typ.method, METHOD_REFRESH);
    let original_integrity_key = client.relays[&relayed_addr].long_term_integrity_key.clone();
    client.update_credentials("rotated-user".to_owned(), "rotated-password".to_owned())?;

    let timeout_id = first_message.transaction_id;
    while let Some(deadline) = client
        .tr_map
        .find(&timeout_id)
        .and_then(Transaction::poll_timeout)
    {
        assert!(deadline < t(600), "transaction must end before allocation expiry");
        client.handle_timeout(deadline)?;
        while let Some(transmit) = client.poll_write() {
            let mut message = Message::new();
            message.raw = transmit.message.to_vec();
            message.decode()?;
            assert_eq!(message.transaction_id, timeout_id, "only UDP retransmissions are sent before backoff");
        }
    }

    let retry_deadline = client
        .relay(relayed_addr)?
        .poll_timeout()
        .expect("pending retry contributes a deadline");
    client.handle_timeout(retry_deadline)?;
    let retry = client.poll_write().expect("a bounded Refresh retry");
    let retry_time = retry.now;
    let mut retry_message = Message::new();
    retry_message.raw = retry.message.to_vec();
    retry_message.decode()?;
    assert!(retry_time < t(600), "retry is scheduled before allocation expiry");
    assert_ne!(retry_message.transaction_id, timeout_id);
    assert!(matches!(
        client
            .tr_map
            .find(&retry_message.transaction_id)
            .map(|transaction| &transaction.transaction_type),
        Some(TransactionType::RefreshRequest { timeout_retry_count: 1, .. })
    ));
    assert_eq!(
        client
            .tr_map
            .find(&retry_message.transaction_id)
            .expect("retry transaction remains tracked")
            .request_id,
        timeout_id,
        "wire retries retain the original logical allocation identity"
    );
    assert_eq!(
        TextAttribute::get_from_as(&retry_message, ATTR_USERNAME)?.text,
        TextAttribute::get_from_as(&first_message, ATTR_USERNAME)?.text,
        "retry keeps the allocation's original username"
    );
    assert_ne!(
        TextAttribute::get_from_as(&retry_message, ATTR_USERNAME)?.text,
        "rotated-user",
        "retry does not switch authentication identity mid-allocation"
    );
    assert_eq!(
        TextAttribute::get_from_as(&retry_message, ATTR_REALM)?.text,
        TextAttribute::get_from_as(&first_message, ATTR_REALM)?.text,
        "retry keeps the allocation's realm"
    );
    assert_eq!(
        Nonce::get_from_as(&retry_message, ATTR_NONCE)?.text,
        Nonce::get_from_as(&first_message, ATTR_NONCE)?.text,
        "retry uses the current allocation nonce"
    );
    let crypto_provider = test_crypto_provider();
    MessageIntegrity::check(&mut first_message, &original_integrity_key, crypto_provider.crypto())?;
    MessageIntegrity::check(&mut retry_message, &original_integrity_key, crypto_provider.crypto())?;

    Ok(())
}

#[test]
fn test_allocation_refresh_timeout_waits_for_backoff_before_retry() -> Result<()> {
    let base = Instant::now();
    let t = |millis| base + Duration::from_millis(millis);
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "original-user".to_owned(),
            password: "original-password".to_owned(),
            realm: "original-realm".to_owned(),
            rto_in_ms: 200,
            ..Default::default()
        },
        test_crypto_provider(),
    )?;
    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "original-allocation-nonce".to_owned()),
            Duration::from_secs(600),
            None,
        ),
    );
    let original_expiry = t(600_000);

    client.relay(relayed_addr)?.handle_timeout(t(300_000));
    let first = client.poll_write().expect("scheduled Refresh request");
    let mut first_message = Message::new();
    first_message.raw = first.message.to_vec();
    first_message.decode()?;
    let first_id = first_message.transaction_id;
    client
        .relay(relayed_addr)?
        .refresh_allocation(t(300_001), Duration::from_secs(600))?;
    assert!(client.poll_write().is_none(), "a second logical Refresh is suppressed while one is active");
    assert_eq!(client.tr_map.size(), 1, "only one Refresh transaction remains in flight");
    let mut last_timeout = None;

    while let Some(deadline) = client
        .tr_map
        .find(&first_id)
        .and_then(Transaction::poll_timeout)
    {
        last_timeout = Some(deadline);
        client.handle_timeout(deadline)?;
        while let Some(transmit) = client.poll_write() {
            let mut message = Message::new();
            message.raw = transmit.message.to_vec();
            message.decode()?;
            assert_eq!(
                message.transaction_id, first_id,
                "the logical retry must wait for its backoff deadline"
            );
        }
    }

    let last_timeout = last_timeout.expect("the original request reaches its timeout");
    let stale_success = refresh_success_response(first_id, Duration::from_secs(900));
    client.handle_stun_message(last_timeout + Duration::from_millis(500), &stale_success.raw)?;
    assert_eq!(
        client.relays[&relayed_addr].allocation_expires_at,
        original_expiry,
        "a late response for the removed transaction cannot extend the allocation"
    );
    let peer_addr: SocketAddr = "192.0.2.44:9000".parse().unwrap();
    client.relay(relayed_addr)?.create_permission(last_timeout, peer_addr)?;
    let permission = client.poll_write().expect("concurrent permission request");
    let mut permission_message = Message::new();
    permission_message.raw = permission.message.to_vec();
    permission_message.decode()?;
    assert_eq!(permission_message.typ.method, METHOD_CREATE_PERMISSION);
    let nonce_challenge = turn_error_response(
        METHOD_CREATE_PERMISSION,
        permission_message.transaction_id,
        438,
        "nonce-current",
    );
    client.handle_stun_message(last_timeout, &nonce_challenge.raw)?;
    let permission_retry = client.poll_write().expect("permission retry applies newer nonce");
    let mut permission_retry_message = Message::new();
    permission_retry_message.raw = permission_retry.message.to_vec();
    permission_retry_message.decode()?;
    assert_eq!(
        Nonce::get_from_as(&permission_retry_message, ATTR_NONCE)?.text,
        "nonce-current"
    );
    let mut permission_success = Message::new();
    permission_success.build(&[
        Box::new(permission_retry_message.transaction_id),
        Box::new(MessageType::new(METHOD_CREATE_PERMISSION, CLASS_SUCCESS_RESPONSE)),
    ])?;
    client.handle_stun_message(last_timeout, &permission_success.raw)?;
    assert!(client.poll_write().is_none(), "no retry is sent at timeout");
    let retry_deadline = client
        .relay(relayed_addr)?
        .poll_timeout()
        .expect("pending retry contributes a deadline");
    assert_eq!(
        retry_deadline.saturating_duration_since(last_timeout),
        Duration::from_secs(1)
    );

    client.handle_timeout(retry_deadline)?;
    let retry = client.poll_write().expect("retry starts at backoff deadline");
    let mut retry_message = Message::new();
    retry_message.raw = retry.message.to_vec();
    retry_message.decode()?;
    assert_eq!(retry_message.typ.method, METHOD_REFRESH);
    assert_ne!(retry_message.transaction_id, first_id);
    assert_eq!(
        Nonce::get_from_as(&retry_message, ATTR_NONCE)?.text,
        "nonce-current",
        "a delayed timeout retry picks up a nonce advanced by another live transaction"
    );
    assert!(matches!(
        client
            .tr_map
            .find(&retry_message.transaction_id)
            .map(|transaction| &transaction.transaction_type),
        Some(TransactionType::RefreshRequest { timeout_retry_count: 1, .. })
    ));
    assert_eq!(
        client.relays[&relayed_addr].lifetime,
        Duration::from_secs(600),
        "a timeout must not extend the server-confirmed allocation lifetime"
    );
    assert_eq!(
        client.relays[&relayed_addr].allocation_expires_at,
        original_expiry,
        "a timeout must not move the confirmed allocation expiry"
    );

    let success_at = retry.now + Duration::from_secs(2);
    let successful_lifetime = Duration::from_secs(500);
    let response = refresh_success_response(retry_message.transaction_id, successful_lifetime);
    client.handle_stun_message(success_at, &response.raw)?;
    assert_eq!(client.relays[&relayed_addr].lifetime, successful_lifetime);
    assert_eq!(
        client.relays[&relayed_addr].allocation_expires_at,
        success_at + successful_lifetime,
        "only a confirmed success response may advance allocation expiry"
    );

    client
        .relay(relayed_addr)?
        .handle_timeout(success_at + successful_lifetime / 2);
    let next_refresh = client.poll_write().expect("next scheduled Refresh starts");
    let mut next_message = Message::new();
    next_message.raw = next_refresh.message.to_vec();
    next_message.decode()?;
    assert!(matches!(
        client
            .tr_map
            .find(&next_message.transaction_id)
            .map(|transaction| &transaction.transaction_type),
        Some(TransactionType::RefreshRequest {
            timeout_retry_count: 0,
            ..
        })
    ), "success resets timeout retry state for the next scheduled refresh");

    Ok(())
}

#[test]
fn test_allocation_refresh_retry_is_not_scheduled_past_usable_deadline() -> Result<()> {
    let base = Instant::now();
    let t = |millis| base + Duration::from_millis(millis);
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "original-user".to_owned(),
            password: "original-password".to_owned(),
            realm: "original-realm".to_owned(),
            rto_in_ms: 200,
            ..Default::default()
        },
        test_crypto_provider(),
    )?;
    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "original-allocation-nonce".to_owned()),
            Duration::from_secs(30),
            None,
        ),
    );

    client.relay(relayed_addr)?.handle_timeout(t(15_000));
    let first = client.poll_write().expect("scheduled Refresh request");
    let mut first_message = Message::new();
    first_message.raw = first.message.to_vec();
    first_message.decode()?;
    let first_id = first_message.transaction_id;
    let mut new_refresh_ids = Vec::new();

    while let Some(deadline) = client
        .tr_map
        .find(&first_id)
        .and_then(Transaction::poll_timeout)
    {
        client.handle_timeout(deadline)?;
        while let Some(transmit) = client.poll_write() {
            let mut message = Message::new();
            message.raw = transmit.message.to_vec();
            message.decode()?;
            if message.typ.method == METHOD_REFRESH && message.transaction_id != first_id {
                new_refresh_ids.push(message.transaction_id);
            }
        }
    }

    assert!(
        new_refresh_ids.is_empty(),
        "a retry whose backoff plus transaction budget cannot fit before expiry must be rejected"
    );
    Ok(())
}

#[test]
fn test_close_cancels_pending_allocation_refresh_retry() -> Result<()> {
    let base = Instant::now();
    let t = |millis| base + Duration::from_millis(millis);
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "original-user".to_owned(),
            password: "original-password".to_owned(),
            realm: "original-realm".to_owned(),
            rto_in_ms: 200,
            ..Default::default()
        },
        test_crypto_provider(),
    )?;
    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "original-allocation-nonce".to_owned()),
            Duration::from_secs(600),
            None,
        ),
    );

    client.relay(relayed_addr)?.handle_timeout(t(300_000));
    let first = client.poll_write().expect("scheduled Refresh request");
    let mut first_message = Message::new();
    first_message.raw = first.message.to_vec();
    first_message.decode()?;
    let first_id = first_message.transaction_id;
    let mut last_timeout = None;
    while let Some(deadline) = client
        .tr_map
        .find(&first_id)
        .and_then(Transaction::poll_timeout)
    {
        last_timeout = Some(deadline);
        client.handle_timeout(deadline)?;
        while client.poll_write().is_some() {}
    }
    let last_timeout = last_timeout.expect("initial Refresh times out");
    let retry_at = client
        .relay(relayed_addr)?
        .poll_timeout()
        .expect("timeout scheduled a delayed retry");
    assert_eq!(retry_at, last_timeout + Duration::from_secs(1));

    let close_at = last_timeout + Duration::from_millis(500);
    client.relay(relayed_addr)?.close(close_at)?;
    let close = client.poll_write().expect("close Refresh is sent");
    let mut close_message = Message::new();
    close_message.raw = close.message.to_vec();
    close_message.decode()?;
    assert_eq!(close_message.typ.method, METHOD_REFRESH);
    let mut close_lifetime = crate::proto::lifetime::Lifetime::default();
    close_lifetime.get_from(&close_message)?;
    assert_eq!(close_lifetime.0, Duration::ZERO);
    assert_ne!(close_message.transaction_id, first_id, "close fences the old operation");
    assert_eq!(client.relay(relayed_addr)?.poll_timeout(), None, "closing cancels relay timers");
    client.relay(relayed_addr)?.close(close_at + Duration::from_millis(1))?;
    assert!(client.poll_write().is_none(), "repeated close does not queue a duplicate Refresh");
    assert_eq!(client.tr_map.size(), 1, "only the original close operation remains active");
    let stale_success = refresh_success_response(first_id, Duration::from_secs(900));
    client.handle_stun_message(close_at + Duration::from_millis(100), &stale_success.raw)?;
    assert_eq!(
        client.relays[&relayed_addr].allocation_expires_at,
        t(600_000),
        "a callback from the superseded request cannot mutate the allocation after close"
    );

    client.handle_timeout(retry_at)?;
    while let Some(transmit) = client.poll_write() {
        let mut message = Message::new();
        message.raw = transmit.message.to_vec();
        message.decode()?;
        assert_eq!(message.transaction_id, close_message.transaction_id);
        let mut lifetime = crate::proto::lifetime::Lifetime::default();
        lifetime.get_from(&message)?;
        assert_eq!(lifetime.0, Duration::ZERO, "the canceled renewal must not be sent after close");
    }
    Ok(())
}

#[test]
fn test_client_shutdown_cancels_pending_allocation_refresh_retry() -> Result<()> {
    let base = Instant::now();
    let t = |millis| base + Duration::from_millis(millis);
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "original-user".to_owned(),
            password: "original-password".to_owned(),
            realm: "original-realm".to_owned(),
            rto_in_ms: 200,
            ..Default::default()
        },
        test_crypto_provider(),
    )?;
    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "original-allocation-nonce".to_owned()),
            Duration::from_secs(600),
            None,
        ),
    );
    client.relay(relayed_addr)?.handle_timeout(t(300_000));
    let first = client.poll_write().expect("scheduled Refresh request");
    let mut first_message = Message::new();
    first_message.raw = first.message.to_vec();
    first_message.decode()?;
    let first_id = first_message.transaction_id;
    let mut last_timeout = None;
    while let Some(deadline) = client
        .tr_map
        .find(&first_id)
        .and_then(Transaction::poll_timeout)
    {
        last_timeout = Some(deadline);
        client.handle_timeout(deadline)?;
        while client.poll_write().is_some() {}
    }
    let retry_at = client
        .relay(relayed_addr)?
        .poll_timeout()
        .expect("timed-out Refresh scheduled a bounded retry");
    assert!(retry_at > last_timeout.expect("Refresh timed out"));

    Protocol::close(&mut client)?;
    assert!(client.poll_write().is_none(), "shutdown clears queued writes");
    assert_eq!(client.poll_timeout(), None, "shutdown removes relay and transaction deadlines");
    client.handle_timeout(retry_at)?;
    assert!(client.poll_write().is_none(), "shutdown prevents a delayed retry from escaping");
    Ok(())
}

#[test]
fn test_allocation_refresh_timeout_retries_are_bounded_and_skip_release() -> Result<()> {
    let base = Instant::now();
    let t = |secs| base + Duration::from_secs(secs);
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username: "original-user".to_owned(),
            password: "original-password".to_owned(),
            realm: "original-realm".to_owned(),
            rto_in_ms: 200,
            ..Default::default()
        },
        test_crypto_provider(),
    )?;
    let relayed_addr: RelayedAddr = "127.0.0.1:50000".parse().unwrap();
    client.relays.insert(
        relayed_addr,
        RelayState::new(
            t(0),
            relayed_addr,
            vec![0u8; 16],
            Nonce::new(ATTR_NONCE, "original-allocation-nonce".to_owned()),
            Duration::from_secs(600),
            None,
        ),
    );
    client.relay(relayed_addr)?.handle_timeout(t(300));
    let first = client.poll_write().expect("scheduled Refresh request");
    let mut first_message = Message::new();
    first_message.raw = first.message.to_vec();
    first_message.decode()?;
    let mut current_id = first_message.transaction_id;
    let mut refresh_requests_sent = 1;

    for retry_index in 0..=super::relay::MAX_REFRESH_TIMEOUT_RETRIES {
        while let Some(deadline) = client
            .tr_map
            .find(&current_id)
            .and_then(Transaction::poll_timeout)
        {
            assert!(deadline < t(600), "all recovery attempts precede allocation expiry");
            client.handle_timeout(deadline)?;
            while let Some(transmit) = client.poll_write() {
                let mut message = Message::new();
                message.raw = transmit.message.to_vec();
                message.decode()?;
                assert_eq!(message.typ.method, METHOD_REFRESH);
                assert_eq!(message.transaction_id, current_id, "only this transaction retransmits before timeout");
            }
        }

        if retry_index < super::relay::MAX_REFRESH_TIMEOUT_RETRIES {
            let retry_at = client
                .relay(relayed_addr)?
                .poll_timeout()
                .expect("pending retry contributes a deadline");
            assert!(retry_at < t(600), "retry backoff precedes allocation expiry");
            client.handle_timeout(retry_at)?;
            let retry = client.poll_write().expect("retry starts after the backoff");
            assert!(retry.now < t(600), "retry must precede allocation expiry");
            let mut retry_message = Message::new();
            retry_message.raw = retry.message.to_vec();
            retry_message.decode()?;
            current_id = retry_message.transaction_id;
            refresh_requests_sent += 1;
        } else {
            assert!(
                client.relay(relayed_addr)?.poll_timeout().expect("permission timer remains") == t(420),
                "no retry deadline remains after the configured bound"
            );
            assert!(client.poll_write().is_none(), "the timeout retry count is a hard limit");
        }
    }
    assert_eq!(
        refresh_requests_sent,
        usize::from(super::relay::MAX_REFRESH_TIMEOUT_RETRIES) + 1,
        "the initial request plus the configured retries is the hard limit"
    );

    client.relay(relayed_addr)?.close(t(340))?;
    let close_request = client.poll_write().expect("LIFETIME=0 close Refresh");
    let mut close_message = Message::new();
    close_message.raw = close_request.message.to_vec();
    close_message.decode()?;
    let close_id = close_message.transaction_id;
    let mut logical_refresh_ids = HashSet::from([close_id]);
    while let Some(deadline) = client
        .tr_map
        .find(&close_id)
        .and_then(Transaction::poll_timeout)
    {
        client.handle_timeout(deadline)?;
        while let Some(transmit) = client.poll_write() {
            let mut message = Message::new();
            message.raw = transmit.message.to_vec();
            message.decode()?;
            if message.typ.method == METHOD_REFRESH {
                logical_refresh_ids.insert(message.transaction_id);
            }
        }
    }
    assert_eq!(
        logical_refresh_ids.len(),
        1,
        "timed-out LIFETIME=0 teardown must not start a new logical Refresh"
    );
    Ok(())
}
