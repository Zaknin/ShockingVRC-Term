use super::*;
use sansio::Protocol;
use std::net::{SocketAddr, UdpSocket};

const TEST_RELAY: &str = "127.0.0.1:50000";
const TEST_PEER: &str = "192.0.2.44:9000";
const TEST_USERNAME: &str = "allocation-user";
const TEST_REALM: &str = "turn.example.test";
const TEST_KEY: &[u8] = b"allocation authentication key";
const MAX_STALE_NONCE_RETRIES: usize = 3;

fn test_crypto_provider() -> Arc<dyn crypto::RTCCryptoProvider> {
    crypto::default_provider().expect("built-in crypto provider is enabled for tests")
}

fn test_client() -> (Client, Instant, RelayedAddr, SocketAddr) {
    let now = Instant::now();
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind test socket");
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: "127.0.0.1:3478".to_owned(),
            local_addr: socket.local_addr().expect("local address"),
            transport_protocol: TransportProtocol::UDP,
            username: TEST_USERNAME.to_owned(),
            password: "test-password".to_owned(),
            realm: TEST_REALM.to_owned(),
            software: "TURN stale nonce test".to_owned(),
            rto_in_ms: 100,
            ..ClientConfig::default()
        },
        test_crypto_provider(),
    )
    .expect("construct test client");
    let relay_addr = TEST_RELAY.parse().expect("relay address");
    let peer_addr = TEST_PEER.parse().expect("peer address");
    client.relays.insert(
        relay_addr,
        RelayState::new(
            now,
            relay_addr,
            TEST_KEY.to_vec(),
            Nonce::new(ATTR_NONCE, "nonce-original".to_owned()),
            Duration::from_secs(600),
            None,
        ),
    );
    (client, now, relay_addr, peer_addr)
}

fn decode(raw: &[u8]) -> Message {
    let mut message = Message::new();
    message.raw = raw.to_vec();
    message.decode().expect("decode STUN message");
    message
}

fn next_request(client: &mut Client) -> Message {
    let transmit = client.poll_write().expect("TURN request is queued immediately");
    assert_eq!(transmit.transport.peer_addr, "127.0.0.1:3478".parse().unwrap());
    decode(&transmit.message)
}

fn response(method: Method, transaction_id: TransactionId, error_code: Option<u16>, nonce: Option<&str>) -> Message {
    let class = if error_code.is_some() {
        CLASS_ERROR_RESPONSE
    } else {
        CLASS_SUCCESS_RESPONSE
    };
    let mut setters: Vec<Box<dyn Setter>> = vec![Box::new(transaction_id), Box::new(MessageType::new(method, class))];
    if let Some(error_code) = error_code {
        setters.push(Box::new(ErrorCodeAttribute {
            code: stun::error_code::ErrorCode(error_code),
            reason: format!("test error {error_code}").into_bytes(),
        }));
    }
    if let Some(nonce) = nonce {
        setters.push(Box::new(Nonce::new(ATTR_NONCE, nonce.to_owned())));
    }
    if method == METHOD_REFRESH && error_code.is_none() {
        setters.push(Box::new(crate::proto::lifetime::Lifetime(Duration::from_secs(600))));
    }
    let mut message = Message::new();
    message.build(&setters).expect("build response");
    message
}

fn respond(client: &mut Client, now: Instant, message: Message) -> Result<()> {
    client.handle_stun_message(now, &message.raw)
}

fn permit_peer(client: &mut Client, now: Instant, relay_addr: RelayedAddr, peer_addr: SocketAddr) -> Result<()> {
    let request_id = client
        .relay(relay_addr)?
        .create_permission(now, peer_addr)?
        .expect("permission request");
    let request = next_request(client);
    respond(
        client,
        now,
        response(METHOD_CREATE_PERMISSION, request.transaction_id, None, None),
    )?;
    assert!(matches!(
        client.poll_event(),
        Some(Event::CreatePermissionResponse(id, peer)) if id == request_id && peer == peer_addr
    ));
    Ok(())
}

fn assert_request_identity(message: &Message, nonce: &str) {
    assert_eq!(message.typ.class, CLASS_REQUEST);
    assert_eq!(
        TextAttribute::get_from_as(message, ATTR_USERNAME)
            .expect("USERNAME")
            .text,
        TEST_USERNAME
    );
    assert_eq!(TextAttribute::get_from_as(message, ATTR_REALM).expect("REALM").text, TEST_REALM);
    assert_eq!(TextAttribute::get_from_as(message, ATTR_NONCE).expect("NONCE").text, nonce);
    assert!(message.get(ATTR_MESSAGE_INTEGRITY).is_ok(), "request stays authenticated");
    let mut authenticated = decode(&message.raw);
    let provider = test_crypto_provider();
    MessageIntegrity::check(&mut authenticated, TEST_KEY, provider.crypto())
        .expect("retry must use the allocation's original authentication key");
}

#[test]
fn allocation_refresh_438_retries_immediately_and_only_success_updates_lifetime() -> Result<()> {
    let (mut client, now, relay_addr, _) = test_client();
    client
        .relay(relay_addr)?
        .refresh_allocation(now, Duration::from_secs(600))?;
    let first = next_request(&mut client);
    let retry_at = now + Duration::from_secs(1);

    respond(
        &mut client,
        retry_at,
        response(METHOD_REFRESH, first.transaction_id, Some(438), Some("nonce-fresh")),
    )?;

    let retry = next_request(&mut client);
    assert_eq!(retry.typ.method, METHOD_REFRESH);
    assert_ne!(
        retry.transaction_id, first.transaction_id,
        "wire retry needs a new transaction id"
    );
    assert_request_identity(&retry, "nonce-fresh");
    let retry_transaction = client
        .tr_map
        .find(&retry.transaction_id)
        .expect("retry transaction tracked");
    assert_eq!(retry_transaction.request_id, first.transaction_id);
    assert_eq!(retry_transaction.local_addr, client.local_addr);
    assert_eq!(retry_transaction.peer_addr, "127.0.0.1:3478".parse().unwrap());
    assert_eq!(retry_transaction.transport_protocol, TransportProtocol::UDP);
    assert_eq!(retry_transaction.timeout, retry_at + Duration::from_millis(100));
    assert_eq!(
        client.relays.get(&relay_addr).expect("allocation preserved").lifetime,
        Duration::from_secs(600),
        "438 must not be reported as a successful Refresh"
    );
    assert!(client.poll_event().is_none(), "no success event before a success response");

    respond(
        &mut client,
        retry_at,
        response(METHOD_REFRESH, retry.transaction_id, None, None),
    )?;
    assert_eq!(client.relays.get(&relay_addr).unwrap().nonce.text, "nonce-fresh");
    assert_eq!(client.relays.get(&relay_addr).unwrap().lifetime, Duration::from_secs(600));
    assert!(client.poll_write().is_none(), "retry is not deferred to the scheduled refresh");
    Ok(())
}

#[test]
fn permission_438_retries_same_peer_and_correlates_success_to_original_request() -> Result<()> {
    let (mut client, now, relay_addr, peer_addr) = test_client();
    let original_id = client
        .relay(relay_addr)?
        .create_permission(now, peer_addr)?
        .expect("permission request");
    let first = next_request(&mut client);
    assert_eq!(first.typ.method, METHOD_CREATE_PERMISSION);

    respond(
        &mut client,
        now + Duration::from_secs(1),
        response(
            METHOD_CREATE_PERMISSION,
            first.transaction_id,
            Some(438),
            Some("permission-nonce-2"),
        ),
    )?;
    let retry = next_request(&mut client);
    assert_eq!(retry.typ.method, METHOD_CREATE_PERMISSION);
    assert_ne!(retry.transaction_id, first.transaction_id);
    assert_request_identity(&retry, "permission-nonce-2");
    let mut retry_peer = PeerAddress::default();
    retry_peer.get_from(&retry)?;
    assert_eq!(SocketAddr::new(retry_peer.ip, retry_peer.port), peer_addr);

    respond(
        &mut client,
        now + Duration::from_secs(2),
        response(METHOD_CREATE_PERMISSION, retry.transaction_id, None, None),
    )?;
    assert!(matches!(
        client.poll_event(),
        Some(Event::CreatePermissionResponse(id, peer)) if id == original_id && peer == peer_addr
    ));
    Ok(())
}

#[test]
fn permission_refresh_438_retries_the_same_peer_set() -> Result<()> {
    let (mut client, now, relay_addr, peer_addr) = test_client();
    let create_id = client
        .relay(relay_addr)?
        .create_permission(now, peer_addr)?
        .expect("permission request");
    let create = next_request(&mut client);
    respond(
        &mut client,
        now,
        response(METHOD_CREATE_PERMISSION, create.transaction_id, None, None),
    )?;
    assert!(matches!(client.poll_event(), Some(Event::CreatePermissionResponse(id, _)) if id == create_id));

    client.relay(relay_addr)?.refresh_permissions(now)?;
    let refresh = next_request(&mut client);
    assert_eq!(refresh.typ.method, METHOD_CREATE_PERMISSION);
    respond(
        &mut client,
        now + Duration::from_secs(1),
        response(
            METHOD_CREATE_PERMISSION,
            refresh.transaction_id,
            Some(438),
            Some("perm-refresh-nonce"),
        ),
    )?;

    let retry = next_request(&mut client);
    assert_eq!(retry.typ.method, METHOD_CREATE_PERMISSION);
    assert_request_identity(&retry, "perm-refresh-nonce");
    let mut retry_peer = PeerAddress::default();
    retry_peer.get_from(&retry)?;
    assert_eq!(SocketAddr::new(retry_peer.ip, retry_peer.port), peer_addr);
    respond(
        &mut client,
        now + Duration::from_secs(2),
        response(METHOD_CREATE_PERMISSION, retry.transaction_id, None, None),
    )?;
    assert!(
        client.poll_event().is_none(),
        "background permission refresh has no create event"
    );
    client
        .relay(relay_addr)?
        .send_to(now, b"permission-still-valid", peer_addr)?;
    Ok(())
}

#[test]
fn channel_bind_438_retries_same_peer_and_channel_number() -> Result<()> {
    let (mut client, now, relay_addr, peer_addr) = test_client();
    permit_peer(&mut client, now, relay_addr, peer_addr)?;

    client.relay(relay_addr)?.send_to(now, b"hello", peer_addr)?;
    let first = next_request(&mut client);
    assert_eq!(first.typ.method, METHOD_CHANNEL_BIND);
    let send_indication = next_request(&mut client);
    assert_eq!(send_indication.typ.method, METHOD_SEND);
    let mut first_channel = ChannelNumber::default();
    first_channel.get_from(&first)?;
    let channel = first_channel.0;

    respond(
        &mut client,
        now + Duration::from_secs(1),
        response(METHOD_CHANNEL_BIND, first.transaction_id, Some(438), Some("channel-nonce-2")),
    )?;
    let retry = next_request(&mut client);
    assert_eq!(retry.typ.method, METHOD_CHANNEL_BIND);
    assert_ne!(retry.transaction_id, first.transaction_id);
    assert_request_identity(&retry, "channel-nonce-2");
    let mut retry_channel = ChannelNumber::default();
    retry_channel.get_from(&retry)?;
    assert_eq!(retry_channel.0, channel);
    let mut retry_peer = PeerAddress::default();
    retry_peer.get_from(&retry)?;
    assert_eq!(SocketAddr::new(retry_peer.ip, retry_peer.port), peer_addr);

    respond(
        &mut client,
        now + Duration::from_secs(2),
        response(METHOD_CHANNEL_BIND, retry.transaction_id, None, None),
    )?;
    let binding = client.binding_mgr.find_by_addr(&peer_addr).expect("binding retained");
    assert_eq!(binding.number, channel);
    assert_eq!(binding.state(), BindingState::Ready);
    Ok(())
}

#[test]
fn channel_bind_401_and_441_are_reported_and_do_not_retry() -> Result<()> {
    for code in [401, 441] {
        let (mut client, now, relay_addr, peer_addr) = test_client();
        permit_peer(&mut client, now, relay_addr, peer_addr)?;
        client.relay(relay_addr)?.send_to(now, b"hello", peer_addr)?;
        let request = next_request(&mut client);
        assert_eq!(request.typ.method, METHOD_CHANNEL_BIND);
        let _send_indication = next_request(&mut client);

        respond(
            &mut client,
            now + Duration::from_secs(1),
            response(METHOD_CHANNEL_BIND, request.transaction_id, Some(code), None),
        )?;
        assert!(client.poll_write().is_none(), "401 and 441 must not be retried as stale nonce");
        assert!(matches!(
            client.poll_event(),
            Some(Event::ChannelBindError(id, peer, _))
                if id == request.transaction_id && peer == peer_addr
        ));
        assert!(client.binding_mgr.find_by_addr(&peer_addr).is_none());
        assert!(client.relays.contains_key(&relay_addr));
    }
    Ok(())
}

#[test]
fn channel_bind_exhaustion_keeps_the_existing_channel_number() -> Result<()> {
    let (mut client, now, relay_addr, peer_addr) = test_client();
    permit_peer(&mut client, now, relay_addr, peer_addr)?;
    client.relay(relay_addr)?.send_to(now, b"hello", peer_addr)?;
    let mut request = next_request(&mut client);
    let _send_indication = next_request(&mut client);
    let mut first_channel = ChannelNumber::default();
    first_channel.get_from(&request)?;
    let channel = first_channel.0;

    for retry_index in 0..=MAX_STALE_NONCE_RETRIES {
        respond(
            &mut client,
            now + Duration::from_secs(retry_index as u64 + 1),
            response(
                METHOD_CHANNEL_BIND,
                request.transaction_id,
                Some(438),
                Some(&format!("bind-nonce-{}", retry_index + 1)),
            ),
        )?;
        if retry_index < MAX_STALE_NONCE_RETRIES {
            request = next_request(&mut client);
            let mut retry_channel = ChannelNumber::default();
            retry_channel.get_from(&request)?;
            assert_eq!(retry_channel.0, channel);
        }
    }
    assert!(client.poll_write().is_none());
    assert!(matches!(
        client.poll_event(),
        Some(Event::ChannelBindError(id, peer, _))
            if id != request.transaction_id && peer == peer_addr
    ));
    let binding = client
        .binding_mgr
        .find_by_addr(&peer_addr)
        .expect("channel map retained");
    assert_eq!(binding.number, channel);
    assert_eq!(binding.state(), BindingState::Idle);
    Ok(())
}

#[test]
fn channel_bind_timeout_releases_request_state_but_keeps_channel_number() -> Result<()> {
    let (mut client, now, relay_addr, peer_addr) = test_client();
    permit_peer(&mut client, now, relay_addr, peer_addr)?;
    client.relay(relay_addr)?.send_to(now, b"hello", peer_addr)?;
    let request = next_request(&mut client);
    let _send_indication = next_request(&mut client);
    let mut request_channel = ChannelNumber::default();
    request_channel.get_from(&request)?;
    let channel = request_channel.0;

    while let Some(deadline) = client.poll_timeout() {
        if deadline >= now + Duration::from_secs(120) {
            return Err(Error::Other("test unexpectedly reached permission timer".to_owned()));
        }
        Protocol::handle_timeout(&mut client, deadline)?;
        if client.tr_map.size() == 0 {
            break;
        }
    }
    assert_eq!(client.tr_map.size(), 0);
    let timeout_event = client.poll_event();
    assert!(
        matches!(
            timeout_event.as_ref(),
            Some(Event::TransactionTimeout(id)) if *id == request.transaction_id
        ),
        "unexpected first event after timeout: {timeout_event:?}"
    );
    assert!(matches!(
        client.poll_event(),
        Some(Event::ChannelBindError(id, peer, _))
            if id == request.transaction_id && peer == peer_addr
    ));
    let binding = client.binding_mgr.find_by_addr(&peer_addr).expect("binding retained");
    assert_eq!(binding.number, channel);
    assert_eq!(binding.state(), BindingState::Idle);
    Ok(())
}

#[test]
fn stale_nonce_retries_are_bounded_and_final_438_is_an_error() -> Result<()> {
    let (mut client, now, relay_addr, _) = test_client();
    client
        .relay(relay_addr)?
        .refresh_allocation(now, Duration::from_secs(600))?;
    let mut request = next_request(&mut client);

    for retry_index in 0..=MAX_STALE_NONCE_RETRIES {
        let result = respond(
            &mut client,
            now + Duration::from_secs(retry_index as u64 + 1),
            response(
                METHOD_REFRESH,
                request.transaction_id,
                Some(438),
                Some(&format!("nonce-{}", retry_index + 1)),
            ),
        );
        if retry_index == MAX_STALE_NONCE_RETRIES {
            assert!(result.is_err(), "fourth 438 must terminate the logical operation");
            assert!(client.poll_write().is_none(), "no fifth wire request is sent");
        } else {
            result?;
            request = next_request(&mut client);
        }
    }
    assert_eq!(
        client
            .relays
            .get(&relay_addr)
            .expect("allocation remains tracked")
            .lifetime,
        Duration::from_secs(600)
    );
    Ok(())
}

#[test]
fn active_allocation_401_and_441_fail_without_nonce_retry_or_allocation_loss() -> Result<()> {
    for code in [401, 441] {
        let (mut client, now, relay_addr, _) = test_client();
        client
            .relay(relay_addr)?
            .refresh_allocation(now, Duration::from_secs(600))?;
        let request = next_request(&mut client);
        assert!(
            respond(
                &mut client,
                now + Duration::from_secs(1),
                response(METHOD_REFRESH, request.transaction_id, Some(code), None),
            )
            .is_err()
        );
        assert!(client.poll_write().is_none(), "authentication error is not retried");
        assert!(
            client.relays.contains_key(&relay_addr),
            "401/441 do not erase live allocation state"
        );
        assert_eq!(
            client.relay(relay_addr)?.poll_timeout(),
            Some(now + Duration::from_secs(120)),
            "401/441 do not install a timeout retry deadline"
        );
    }
    Ok(())
}

#[test]
fn missing_nonce_and_changed_authentication_do_not_create_bad_retries() -> Result<()> {
    let (mut client, now, relay_addr, _) = test_client();
    client
        .relay(relay_addr)?
        .refresh_allocation(now, Duration::from_secs(600))?;
    let original = next_request(&mut client);

    // A response without NONCE is invalid and cannot be treated as a successful renewal or retry.
    assert!(
        respond(
            &mut client,
            now + Duration::from_secs(1),
            response(METHOD_REFRESH, original.transaction_id, Some(438), None),
        )
        .is_err()
    );
    assert!(client.poll_write().is_none());
    assert_eq!(client.relays[&relay_addr].nonce.text, "nonce-original");
    Ok(())
}

#[test]
fn retry_keeps_the_username_realm_and_key_from_the_challenged_request() -> Result<()> {
    let (mut client, now, relay_addr, _) = test_client();
    client
        .relay(relay_addr)?
        .refresh_allocation(now, Duration::from_secs(600))?;
    let pending = next_request(&mut client);
    client.update_credentials("rotated-user".to_owned(), "rotated-password".to_owned())?;
    respond(
        &mut client,
        now + Duration::from_secs(2),
        response(METHOD_REFRESH, pending.transaction_id, Some(438), Some("valid-new-nonce")),
    )?;
    let retry = next_request(&mut client);
    assert_request_identity(&retry, "valid-new-nonce");
    assert_ne!(
        TextAttribute::get_from_as(&retry, ATTR_USERNAME)?.text,
        "rotated-user",
        "retry uses the auth context that was challenged"
    );
    Ok(())
}

#[test]
fn retry_transaction_timeout_reports_the_original_logical_request_id() -> Result<()> {
    let (mut client, now, relay_addr, _) = test_client();
    client
        .relay(relay_addr)?
        .refresh_allocation(now, Duration::from_secs(600))?;
    let original = next_request(&mut client);
    respond(
        &mut client,
        now + Duration::from_secs(1),
        response(METHOD_REFRESH, original.transaction_id, Some(438), Some("timeout-nonce")),
    )?;
    let retry = next_request(&mut client);
    let retry_transaction = client
        .tr_map
        .find(&retry.transaction_id)
        .expect("retry transaction remains tracked");
    assert_eq!(retry_transaction.request_id, original.transaction_id);
    assert_eq!(retry_transaction.local_addr, client.local_addr);
    assert_eq!(retry_transaction.peer_addr, "127.0.0.1:3478".parse().unwrap());
    assert_eq!(retry_transaction.transport_protocol, TransportProtocol::UDP);

    while let Some(deadline) = client.tr_map.poll_timout() {
        client.tr_map.handle_timeout(deadline);
    }
    assert!(matches!(
        client.poll_event(),
        Some(Event::TransactionTimeout(id)) if id == original.transaction_id
    ));
    Ok(())
}

#[test]
fn allocation_mismatch_437_drops_the_allocation_without_attempting_reallocation() -> Result<()> {
    let (mut client, now, relay_addr, _) = test_client();
    client
        .relay(relay_addr)?
        .refresh_allocation(now, Duration::from_secs(600))?;
    let request = next_request(&mut client);
    assert!(
        respond(
            &mut client,
            now + Duration::from_secs(1),
            response(METHOD_REFRESH, request.transaction_id, Some(437), None),
        )
        .is_err()
    );
    assert!(!client.relays.contains_key(&relay_addr), "437 means this allocation is gone");
    assert!(
        client.poll_write().is_none(),
        "the client must not issue a new Allocate implicitly"
    );
    Ok(())
}

#[test]
fn concurrent_stale_responses_do_not_roll_nonce_back() -> Result<()> {
    let (mut client, now, relay_addr, peer_addr) = test_client();
    permit_peer(&mut client, now, relay_addr, peer_addr)?;
    client
        .relay(relay_addr)?
        .refresh_allocation(now, Duration::from_secs(600))?;
    let refresh = next_request(&mut client);
    client.relay(relay_addr)?.refresh_permissions(now)?;
    let permission = next_request(&mut client);
    assert_eq!(refresh.typ.method, METHOD_REFRESH);
    assert_eq!(permission.typ.method, METHOD_CREATE_PERMISSION);

    respond(
        &mut client,
        now + Duration::from_secs(1),
        response(METHOD_REFRESH, refresh.transaction_id, Some(438), Some("nonce-new")),
    )?;
    let refresh_retry = next_request(&mut client);
    assert_eq!(refresh_retry.typ.method, METHOD_REFRESH);
    assert_request_identity(&refresh_retry, "nonce-new");

    respond(
        &mut client,
        now + Duration::from_secs(2),
        response(METHOD_CREATE_PERMISSION, permission.transaction_id, Some(438), Some("nonce-stale")),
    )?;
    let permission_retry = next_request(&mut client);
    assert_eq!(permission_retry.typ.method, METHOD_CREATE_PERMISSION);
    assert_request_identity(&permission_retry, "nonce-new");
    assert_eq!(client.relays.get(&relay_addr).unwrap().nonce.text, "nonce-new");
    Ok(())
}

#[test]
fn close_cancels_a_queued_stale_nonce_retry() -> Result<()> {
    let (mut client, now, relay_addr, _) = test_client();
    client
        .relay(relay_addr)?
        .refresh_allocation(now, Duration::from_secs(600))?;
    let request = next_request(&mut client);
    respond(
        &mut client,
        now + Duration::from_secs(1),
        response(METHOD_REFRESH, request.transaction_id, Some(438), Some("nonce-retry")),
    )?;
    Protocol::close(&mut client)?;
    assert!(
        client.poll_write().is_none(),
        "close removes a queued retry before it can escape"
    );
    assert_eq!(client.tr_map.size(), 0, "close cancels all outstanding TURN operations");
    Ok(())
}
