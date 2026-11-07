// Added by ShockingVRC-Term contributors on 2026-11-07; see vendor/rtc-turn-0.21.1/PATCHES.md.
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

const TEST_PEER_2: &str = "198.51.100.29:9001";

fn permission_peers(message: &Message) -> Result<Vec<SocketAddr>> {
    let mut peers = message
        .attributes
        .0
        .iter()
        .filter(|attribute| attribute.typ == ATTR_XOR_PEER_ADDRESS)
        .map(|attribute| {
            let mut one_attribute = Message::default();
            one_attribute.attributes.0.push(attribute.clone());
            let mut peer = PeerAddress::default();
            peer.get_from(&one_attribute)?;
            Ok(SocketAddr::new(peer.ip, peer.port))
        })
        .collect::<Result<Vec<_>>>()?;
    peers.sort_unstable();
    Ok(peers)
}

#[test]
fn scheduled_permission_438_retries_immediately_with_the_original_peer_set_and_auth() -> Result<()>
{
    let (mut client, now, relay_addr, first_peer) = test_client();
    let second_peer: SocketAddr = TEST_PEER_2.parse().expect("second peer address");
    permit_peer(&mut client, now, relay_addr, first_peer)?;
    permit_peer(&mut client, now, relay_addr, second_peer)?;
    let mut expected_peers = vec![first_peer, second_peer];
    expected_peers.sort_unstable();

    let scheduled_at = now + Duration::from_secs(120);
    assert_eq!(client.relay(relay_addr)?.poll_timeout(), Some(scheduled_at));
    Protocol::handle_timeout(&mut client, scheduled_at - Duration::from_millis(1))?;
    assert!(
        client.poll_write().is_none(),
        "permission cadence remains 120 seconds"
    );
    Protocol::handle_timeout(&mut client, scheduled_at)?;
    let challenged = next_request(&mut client);
    assert_eq!(challenged.typ.method, METHOD_CREATE_PERMISSION);
    assert_eq!(permission_peers(&challenged)?, expected_peers);
    let challenged_transaction = client
        .tr_map
        .find(&challenged.transaction_id)
        .expect("scheduled CreatePermission is tracked");
    let challenged_peer_set = match &challenged_transaction.transaction_type {
        TransactionType::CreatePermissionRequest {
            relayed_addr: operation_relay,
            peer_addrs,
            peer_addr_opt,
            retry_count,
            ..
        } => {
            assert_eq!(*operation_relay, relay_addr);
            assert!(peer_addr_opt.is_none());
            assert_eq!(*retry_count, 0);
            let mut peers = peer_addrs.clone();
            peers.sort_unstable();
            peers
        }
        _ => panic!("scheduled request is not CreatePermission"),
    };
    assert_eq!(challenged_peer_set, expected_peers);

    // Rotate credentials while the scheduled request is in flight. The challenged operation
    // must retain the username, realm and integrity key it started with.
    client.update_credentials("rotated-user".to_owned(), "rotated-password".to_owned())?;
    respond(
        &mut client,
        scheduled_at + Duration::from_secs(1),
        response(
            METHOD_CREATE_PERMISSION,
            challenged.transaction_id,
            Some(438),
            Some("scheduled-permission-nonce"),
        ),
    )?;

    // A duplicate response for the superseded transaction cannot queue a second retry or
    // roll the current nonce back.
    respond(
        &mut client,
        scheduled_at + Duration::from_millis(1100),
        response(
            METHOD_CREATE_PERMISSION,
            challenged.transaction_id,
            Some(438),
            Some("duplicate-stale-nonce"),
        ),
    )?;
    assert_eq!(
        client.relays[&relay_addr].nonce.text,
        "scheduled-permission-nonce"
    );

    let retry = next_request(&mut client);
    assert_eq!(retry.typ.method, METHOD_CREATE_PERMISSION);
    assert_ne!(retry.transaction_id, challenged.transaction_id);
    assert_eq!(permission_peers(&retry)?, expected_peers);
    assert_request_identity(&retry, "scheduled-permission-nonce");
    assert_ne!(
        TextAttribute::get_from_as(&retry, ATTR_USERNAME)?.text,
        "rotated-user",
        "retry uses the authentication context challenged by the server"
    );
    let retry_transaction = client
        .tr_map
        .find(&retry.transaction_id)
        .expect("retry transaction is tracked");
    assert_eq!(retry_transaction.request_id, challenged.transaction_id);
    assert_eq!(retry_transaction.local_addr, client.local_addr);
    assert_eq!(
        retry_transaction.peer_addr,
        "127.0.0.1:3478".parse().unwrap()
    );
    assert_eq!(retry_transaction.transport_protocol, TransportProtocol::UDP);
    let retry_transaction_peer_set = match &retry_transaction.transaction_type {
        TransactionType::CreatePermissionRequest {
            relayed_addr: operation_relay,
            peer_addrs,
            peer_addr_opt,
            retry_count,
            nonce_generation,
            auth,
        } => {
            assert_eq!(*operation_relay, relay_addr);
            assert!(peer_addr_opt.is_none());
            assert_eq!(*retry_count, 1);
            assert_eq!(*nonce_generation, 1);
            assert_eq!(auth.username.text, TEST_USERNAME);
            assert_eq!(auth.realm.text, TEST_REALM);
            assert_eq!(auth.integrity_key, TEST_KEY);
            let mut peers = peer_addrs.clone();
            peers.sort_unstable();
            peers
        }
        _ => panic!("retry is not CreatePermission"),
    };
    assert_eq!(retry_transaction_peer_set, expected_peers);
    assert!(
        client.poll_event().is_none(),
        "438 does not report a permission refresh success"
    );

    respond(
        &mut client,
        scheduled_at + Duration::from_secs(2),
        response(METHOD_CREATE_PERMISSION, retry.transaction_id, None, None),
    )?;
    assert!(
        client.poll_event().is_none(),
        "background refresh has no create event"
    );
    assert!(
        client.poll_write().is_none(),
        "success does not defer work to another timer"
    );
    Ok(())
}

#[test]
fn scheduled_permission_438_retries_stop_at_the_bounded_limit() -> Result<()> {
    let (mut client, now, relay_addr, peer_addr) = test_client();
    permit_peer(&mut client, now, relay_addr, peer_addr)?;
    let scheduled_at = now + Duration::from_secs(120);
    Protocol::handle_timeout(&mut client, scheduled_at)?;
    let mut request = next_request(&mut client);
    for retry_index in 0..=MAX_STALE_NONCE_RETRIES {
        respond(
            &mut client,
            scheduled_at + Duration::from_secs(retry_index as u64 + 1),
            response(
                METHOD_CREATE_PERMISSION,
                request.transaction_id,
                Some(438),
                Some(&format!("scheduled-nonce-{}", retry_index + 1)),
            ),
        )?;
        if retry_index < MAX_STALE_NONCE_RETRIES {
            request = next_request(&mut client);
            assert_ne!(
                request.transaction_id,
                client
                    .tr_map
                    .find(&request.transaction_id)
                    .unwrap()
                    .request_id
            );
        } else {
            assert!(
                client.poll_write().is_none(),
                "no retry follows the bounded final 438"
            );
        }
    }
    assert!(
        client.poll_event().is_none(),
        "a failed refresh never reports success"
    );
    assert_eq!(client.tr_map.size(), 0);
    Ok(())
}

#[test]
fn scheduled_permission_438_does_not_retry_after_allocation_close_or_near_expiry() -> Result<()> {
    let (mut closing_client, now, relay_addr, peer_addr) = test_client();
    permit_peer(&mut closing_client, now, relay_addr, peer_addr)?;
    let scheduled_at = now + Duration::from_secs(120);
    Protocol::handle_timeout(&mut closing_client, scheduled_at)?;
    let permission_request = next_request(&mut closing_client);
    let close_at = scheduled_at + Duration::from_secs(1);
    closing_client.relay(relay_addr)?.close(close_at)?;
    let mut saw_zero_lifetime_close = false;
    while let Some(transmit) = closing_client.poll_write() {
        let close_request = decode(&transmit.message);
        if close_request.typ.method == METHOD_REFRESH {
            let mut lifetime = crate::proto::lifetime::Lifetime::default();
            lifetime.get_from(&close_request)?;
            saw_zero_lifetime_close = lifetime.0.is_zero();
        }
    }
    assert!(
        saw_zero_lifetime_close,
        "allocation close request is in flight"
    );
    respond(
        &mut closing_client,
        close_at + Duration::from_secs(1),
        response(
            METHOD_CREATE_PERMISSION,
            permission_request.transaction_id,
            Some(438),
            Some("closing-nonce"),
        ),
    )?;
    assert!(
        closing_client.poll_write().is_none(),
        "a 438 received after allocation close must not queue CreatePermission"
    );

    let (mut expiring_client, now, relay_addr, peer_addr) = test_client();
    permit_peer(&mut expiring_client, now, relay_addr, peer_addr)?;
    let scheduled_at = now + Duration::from_secs(120);
    expiring_client
        .relays
        .get_mut(&relay_addr)
        .expect("allocation exists")
        .allocation_expires_at = scheduled_at + Duration::from_secs(2);
    Protocol::handle_timeout(&mut expiring_client, scheduled_at)?;
    let permission_request = next_request(&mut expiring_client);
    respond(
        &mut expiring_client,
        scheduled_at + Duration::from_secs(1),
        response(
            METHOD_CREATE_PERMISSION,
            permission_request.transaction_id,
            Some(438),
            Some("near-expiry-nonce"),
        ),
    )?;
    assert!(
        expiring_client.poll_write().is_none(),
        "retry must fit before the last server-confirmed allocation expiry"
    );
    assert!(
        expiring_client.poll_event().is_none(),
        "no success is reported without a successful response"
    );
    Ok(())
}

#[test]
fn scheduled_permission_438_retry_timeout_and_client_close_do_not_report_success() -> Result<()> {
    let (mut timed_client, now, relay_addr, peer_addr) = test_client();
    permit_peer(&mut timed_client, now, relay_addr, peer_addr)?;
    let scheduled_at = now + Duration::from_secs(120);
    Protocol::handle_timeout(&mut timed_client, scheduled_at)?;
    let initial = next_request(&mut timed_client);
    respond(
        &mut timed_client,
        scheduled_at + Duration::from_secs(1),
        response(
            METHOD_CREATE_PERMISSION,
            initial.transaction_id,
            Some(438),
            Some("timeout-nonce"),
        ),
    )?;
    let retry = next_request(&mut timed_client);
    assert!(
        timed_client
            .tr_map
            .find(&retry.transaction_id)
            .and_then(Transaction::poll_timeout)
            .is_some(),
        "retry has a bounded transaction deadline"
    );
    while let Some(deadline) = timed_client
        .tr_map
        .find(&retry.transaction_id)
        .and_then(Transaction::poll_timeout)
    {
        Protocol::handle_timeout(&mut timed_client, deadline)?;
    }
    assert!(
        matches!(timed_client.poll_event(), Some(Event::TransactionTimeout(id)) if id == initial.transaction_id),
        "timeout remains correlated to the logical scheduled operation"
    );
    assert!(
        timed_client.poll_event().is_none(),
        "timeout cannot report refresh success"
    );
    assert!(timed_client.poll_write().is_none());

    let (mut closed_client, now, relay_addr, peer_addr) = test_client();
    permit_peer(&mut closed_client, now, relay_addr, peer_addr)?;
    let scheduled_at = now + Duration::from_secs(120);
    Protocol::handle_timeout(&mut closed_client, scheduled_at)?;
    let initial = next_request(&mut closed_client);
    respond(
        &mut closed_client,
        scheduled_at + Duration::from_secs(1),
        response(
            METHOD_CREATE_PERMISSION,
            initial.transaction_id,
            Some(438),
            Some("cancel-nonce"),
        ),
    )?;
    assert!(
        closed_client.poll_write().is_some(),
        "retry is queued before shutdown"
    );
    Protocol::close(&mut closed_client)?;
    assert!(
        closed_client.poll_write().is_none(),
        "shutdown cancels the queued retry"
    );
    respond(
        &mut closed_client,
        scheduled_at + Duration::from_secs(2),
        response(METHOD_CREATE_PERMISSION, initial.transaction_id, None, None),
    )?;
    assert!(
        closed_client.poll_event().is_none(),
        "a cancelled request cannot report success"
    );
    Ok(())
}

#[test]
fn scheduled_permission_refresh_does_not_retry_permanent_authentication_errors() -> Result<()> {
    for code in [401, 441] {
        let (mut client, now, relay_addr, peer_addr) = test_client();
        permit_peer(&mut client, now, relay_addr, peer_addr)?;
        let scheduled_at = now + Duration::from_secs(120);
        Protocol::handle_timeout(&mut client, scheduled_at)?;
        let request = next_request(&mut client);
        respond(
            &mut client,
            scheduled_at + Duration::from_secs(1),
            response(
                METHOD_CREATE_PERMISSION,
                request.transaction_id,
                Some(code),
                None,
            ),
        )?;
        assert!(
            client.poll_write().is_none(),
            "permanent authentication error {code} is not retried"
        );
        assert!(
            client.poll_event().is_none(),
            "background refresh errors follow existing event policy"
        );
        assert!(
            client.relays.contains_key(&relay_addr),
            "permanent permission error preserves allocation"
        );
    }
    Ok(())
}

fn pending_requests(client: &mut Client) -> Vec<Message> {
    let mut requests = Vec::new();
    while let Some(transmit) = client.poll_write() {
        requests.push(decode(&transmit.message));
    }
    requests
}

#[test]
fn scheduled_permission_and_channel_bind_438_share_nonce_without_rollback() -> Result<()> {
    let (mut client, now, relay_addr, peer_addr) = test_client();
    permit_peer(&mut client, now, relay_addr, peer_addr)?;
    let scheduled_at = now + Duration::from_secs(120);
    Protocol::handle_timeout(&mut client, scheduled_at)?;
    client
        .relay(relay_addr)?
        .send_to(scheduled_at, b"bind channel", peer_addr)?;

    let pending = pending_requests(&mut client);
    let permission = pending
        .iter()
        .find(|message| message.typ.method == METHOD_CREATE_PERMISSION)
        .expect("scheduled permission refresh");
    let channel_bind = pending
        .iter()
        .find(|message| message.typ.method == METHOD_CHANNEL_BIND)
        .expect("channel bind starts alongside permission refresh")
        .clone();
    let permission = permission.clone();
    let mut channel_number = ChannelNumber::default();
    channel_number.get_from(&channel_bind)?;
    let channel = channel_number.0;

    respond(
        &mut client,
        scheduled_at + Duration::from_secs(1),
        response(
            METHOD_CHANNEL_BIND,
            channel_bind.transaction_id,
            Some(438),
            Some("shared-nonce-current"),
        ),
    )?;
    let bind_retry = pending_requests(&mut client)
        .into_iter()
        .find(|message| message.typ.method == METHOD_CHANNEL_BIND)
        .expect("ChannelBind 438 retry is immediate");
    assert_ne!(bind_retry.transaction_id, channel_bind.transaction_id);
    assert_request_identity(&bind_retry, "shared-nonce-current");
    let mut retry_channel = ChannelNumber::default();
    retry_channel.get_from(&bind_retry)?;
    assert_eq!(retry_channel.0, channel);

    // This response belongs to an operation signed with the older nonce. Its nonce must not
    // replace the one installed by the concurrent ChannelBind 438 response.
    respond(
        &mut client,
        scheduled_at + Duration::from_secs(2),
        response(
            METHOD_CREATE_PERMISSION,
            permission.transaction_id,
            Some(438),
            Some("shared-nonce-stale"),
        ),
    )?;
    let permission_retry = pending_requests(&mut client)
        .into_iter()
        .find(|message| message.typ.method == METHOD_CREATE_PERMISSION)
        .expect("scheduled CreatePermission retries immediately");
    assert_ne!(permission_retry.transaction_id, permission.transaction_id);
    assert_request_identity(&permission_retry, "shared-nonce-current");
    assert_eq!(
        client.relays[&relay_addr].nonce.text,
        "shared-nonce-current"
    );
    let permission_retry_transaction = client
        .tr_map
        .find(&permission_retry.transaction_id)
        .expect("permission retry is tracked");
    assert_eq!(
        permission_retry_transaction.request_id,
        permission.transaction_id
    );

    respond(
        &mut client,
        scheduled_at + Duration::from_secs(3),
        response(
            METHOD_CREATE_PERMISSION,
            permission_retry.transaction_id,
            None,
            None,
        ),
    )?;
    respond(
        &mut client,
        scheduled_at + Duration::from_secs(4),
        response(METHOD_CHANNEL_BIND, bind_retry.transaction_id, None, None),
    )?;
    let binding = client
        .binding_mgr
        .find_by_addr(&peer_addr)
        .expect("successful bind retains its peer");
    assert_eq!(binding.number, channel);
    assert_eq!(binding.state(), BindingState::Ready);
    assert!(
        client.poll_event().is_none(),
        "scheduled refresh remains silent on success"
    );
    Ok(())
}

fn coturn_fixture_value(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} must be set for this ignored coturn test"))
}

fn send_pending_to_coturn(client: &mut Client, socket: &UdpSocket) -> Result<Vec<Vec<u8>>> {
    let mut sent = Vec::new();
    while let Some(transmit) = client.poll_write() {
        let bytes = transmit.message.to_vec();
        let written = socket.send_to(&bytes, transmit.transport.peer_addr)?;
        assert_eq!(
            written,
            bytes.len(),
            "complete UDP datagram sent to local coturn"
        );
        sent.push(bytes);
    }
    Ok(sent)
}

fn receive_from_coturn(client: &mut Client, socket: &UdpSocket, now: Instant) -> Result<Message> {
    let mut buf = [0u8; 2048];
    let (len, peer_addr) = socket.recv_from(&mut buf)?;
    let packet = BytesMut::from(&buf[..len]);
    let message = decode(&packet);
    client.handle_read(TransportMessage {
        now,
        transport: TransportContext {
            local_addr: socket.local_addr()?,
            peer_addr,
            transport_protocol: TransportProtocol::UDP,
            ecn: None,
        },
        message: packet,
    })?;
    Ok(message)
}

fn channel_number(message: &Message) -> Result<u16> {
    let mut number = ChannelNumber::default();
    number.get_from(message)?;
    Ok(number.0)
}

fn peer_address(message: &Message) -> Result<SocketAddr> {
    let mut peer = PeerAddress::default();
    peer.get_from(message)?;
    Ok(SocketAddr::new(peer.ip, peer.port))
}

/// Runs a real UDP exchange with a local coturn configured for a two-second nonce lifetime.
/// The first binding succeeds, then the client clock advances beyond its 600-second refresh
/// threshold while wall time advances past the nonce lifetime. The server's 438 response must
/// cause an immediate same-channel retry accepted by coturn; the next client data packet must
/// still carry the original ChannelData number.
#[test]
#[ignore = "requires local coturn 4.6.1 with --stale-nonce=2 and SHOCKINGVRC_TEST_COTURN_* values"]
fn coturn_channel_bind_438_retry_keeps_wire_channel_and_channel_data_mapping() -> Result<()> {
    let turn_addr: SocketAddr = coturn_fixture_value("SHOCKINGVRC_TEST_COTURN_ADDR")
        .parse()
        .expect("valid local coturn socket address");
    assert!(
        turn_addr.ip().is_loopback(),
        "fixture must be loopback-only"
    );

    let username = coturn_fixture_value("SHOCKINGVRC_TEST_COTURN_USERNAME");
    let password = coturn_fixture_value("SHOCKINGVRC_TEST_COTURN_PASSWORD");
    let realm = coturn_fixture_value("SHOCKINGVRC_TEST_COTURN_REALM");
    let peer_addr: SocketAddr = "192.0.2.44:9000"
        .parse()
        .expect("reserved test peer address");
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_read_timeout(Some(Duration::from_secs(4)))?;
    let now = Instant::now();
    let mut client = Client::new(
        ClientConfig {
            turn_serv_addr: turn_addr.to_string(),
            local_addr: socket.local_addr()?,
            transport_protocol: TransportProtocol::UDP,
            username,
            password,
            realm,
            software: "ShockingVRC isolated ChannelBind regression".to_owned(),
            rto_in_ms: 100,
            ..ClientConfig::default()
        },
        test_crypto_provider(),
    )?;

    let allocation_id = client.allocate(now)?;
    let initial_allocate = send_pending_to_coturn(&mut client, &socket)?
        .into_iter()
        .next()
        .expect("initial Allocate is sent");
    let initial_allocate = decode(&initial_allocate);
    assert_eq!(initial_allocate.typ.method, METHOD_ALLOCATE);
    let challenge = receive_from_coturn(&mut client, &socket, Instant::now())?;
    assert_eq!(challenge.typ.method, METHOD_ALLOCATE);
    assert_eq!(challenge.typ.class, CLASS_ERROR_RESPONSE);
    let mut challenge_code = ErrorCodeAttribute::default();
    challenge_code.get_from(&challenge)?;
    assert_eq!(
        challenge_code.code.0, 401,
        "coturn challenges the test allocation"
    );

    let authenticated_allocate = send_pending_to_coturn(&mut client, &socket)?
        .into_iter()
        .map(|packet| decode(&packet))
        .find(|message| message.typ.method == METHOD_ALLOCATE)
        .expect("authenticated Allocate is sent after the challenge");
    assert_eq!(authenticated_allocate.transaction_id, allocation_id);
    let allocated = receive_from_coturn(&mut client, &socket, Instant::now())?;
    assert_eq!(allocated.typ.method, METHOD_ALLOCATE);
    assert_eq!(allocated.typ.class, CLASS_SUCCESS_RESPONSE);
    let relay_addr = match client.poll_event() {
        Some(Event::AllocateResponse(id, relay_addr)) if id == allocation_id => relay_addr,
        other => panic!("expected local coturn allocation success, got {other:?}"),
    };

    let permission_id = client
        .relay(relay_addr)?
        .create_permission(Instant::now(), peer_addr)?
        .expect("permission request queued");
    let mut permission_succeeded = false;
    loop {
        send_pending_to_coturn(&mut client, &socket)?;
        let response = receive_from_coturn(&mut client, &socket, Instant::now())?;
        assert_eq!(response.typ.method, METHOD_CREATE_PERMISSION);
        while let Some(event) = client.poll_event() {
            match event {
                Event::CreatePermissionResponse(id, peer) => {
                    assert_eq!(id, permission_id);
                    assert_eq!(peer, peer_addr);
                    permission_succeeded = true;
                    break;
                }
                Event::CreatePermissionError(_, error) => {
                    panic!("coturn permission failed: {error}")
                }
                Event::TransactionTimeout(id) => panic!("coturn permission timed out: {id:?}"),
                _ => {}
            }
        }
        if permission_succeeded {
            break;
        }
    }

    let initial_bind_at = Instant::now();
    client
        .relay(relay_addr)?
        .send_to(initial_bind_at, b"initial-test-datagram", peer_addr)?;
    let initial_packets = send_pending_to_coturn(&mut client, &socket)?;
    let initial_bind = initial_packets
        .iter()
        .map(|packet| decode(packet))
        .find(|message| message.typ.method == METHOD_CHANNEL_BIND)
        .expect("initial ChannelBind is sent");
    let original_channel = channel_number(&initial_bind)?;
    assert_eq!(original_channel, 0x4000);
    let mut initial_response = receive_from_coturn(&mut client, &socket, Instant::now())?;
    if initial_response.typ.class == CLASS_ERROR_RESPONSE {
        let mut code = ErrorCodeAttribute::default();
        code.get_from(&initial_response)?;
        assert_eq!(
            code.code.0, 438,
            "only a stale nonce can challenge the initial binding"
        );
        let retry = send_pending_to_coturn(&mut client, &socket)?
            .into_iter()
            .map(|packet| decode(&packet))
            .find(|message| message.typ.method == METHOD_CHANNEL_BIND)
            .expect("initial 438 must immediately retry");
        assert_eq!(channel_number(&retry)?, original_channel);
        assert_ne!(retry.transaction_id, initial_bind.transaction_id);
        initial_response = receive_from_coturn(&mut client, &socket, Instant::now())?;
    }
    assert_eq!(initial_response.typ.method, METHOD_CHANNEL_BIND);
    assert_eq!(initial_response.typ.class, CLASS_SUCCESS_RESPONSE);
    let binding = client
        .binding_mgr
        .find_by_addr(&peer_addr)
        .expect("initial binding retained");
    assert_eq!(binding.number, original_channel);
    assert_eq!(binding.state(), BindingState::Ready);

    // Let the server nonce age while moving only the client-side scheduler past the 600-second
    // binding refresh threshold. The real allocation and peer permission remain comfortably alive.
    std::thread::sleep(Duration::from_secs(3));
    let refresh_now = initial_bind_at + Duration::from_secs(601);
    client
        .relay(relay_addr)?
        .send_to(refresh_now, b"refresh-test-datagram", peer_addr)?;
    let refresh_packets = send_pending_to_coturn(&mut client, &socket)?;
    let refresh_bind = refresh_packets
        .iter()
        .map(|packet| decode(packet))
        .find(|message| message.typ.method == METHOD_CHANNEL_BIND)
        .expect("scheduled ChannelBind refresh is sent");
    assert_eq!(channel_number(&refresh_bind)?, original_channel);

    let stale_nonce =
        receive_from_coturn(&mut client, &socket, refresh_now + Duration::from_millis(1))?;
    assert_eq!(stale_nonce.typ.method, METHOD_CHANNEL_BIND);
    assert_eq!(stale_nonce.typ.class, CLASS_ERROR_RESPONSE);
    let mut stale_code = ErrorCodeAttribute::default();
    stale_code.get_from(&stale_nonce)?;
    assert_eq!(
        stale_code.code.0, 438,
        "coturn must actually reject the stale-nonce request"
    );
    assert_eq!(channel_number(&refresh_bind)?, original_channel);
    assert_eq!(peer_address(&refresh_bind)?, peer_addr);
    let fresh_nonce = Nonce::get_from_as(&stale_nonce, ATTR_NONCE)?.text;

    let retry = send_pending_to_coturn(&mut client, &socket)?
        .into_iter()
        .map(|packet| decode(&packet))
        .find(|message| message.typ.method == METHOD_CHANNEL_BIND)
        .expect("438 response queues an immediate retry");
    assert_ne!(
        retry.transaction_id, refresh_bind.transaction_id,
        "retry gets a fresh STUN TID"
    );
    assert_eq!(
        channel_number(&retry)?,
        original_channel,
        "retry preserves channel 0x4000"
    );
    assert_eq!(
        peer_address(&retry)?,
        peer_addr,
        "retry preserves the exact peer tuple"
    );
    assert_eq!(Nonce::get_from_as(&retry, ATTR_NONCE)?.text, fresh_nonce);
    assert!(
        retry.get(ATTR_MESSAGE_INTEGRITY).is_ok(),
        "authenticated retry carries MESSAGE-INTEGRITY"
    );

    let retry_response =
        receive_from_coturn(&mut client, &socket, refresh_now + Duration::from_millis(2))?;
    assert_eq!(retry_response.typ.method, METHOD_CHANNEL_BIND);
    assert_eq!(
        retry_response.typ.class, CLASS_SUCCESS_RESPONSE,
        "coturn accepts the authenticated retry"
    );
    let binding = client
        .binding_mgr
        .find_by_addr(&peer_addr)
        .expect("binding survives retry");
    assert_eq!(binding.number, original_channel);
    assert_eq!(binding.state(), BindingState::Ready);

    client.relay(relay_addr)?.send_to(
        refresh_now + Duration::from_secs(1),
        b"channel-data-test",
        peer_addr,
    )?;
    let channel_data = client.poll_write().expect("post-refresh data is sent");
    assert!(channel_data.message.len() >= 4);
    assert_eq!(
        u16::from_be_bytes([channel_data.message[0], channel_data.message[1]]),
        original_channel,
        "post-refresh ChannelData stays on the confirmed channel"
    );
    socket.send_to(&channel_data.message, channel_data.transport.peer_addr)?;

    // Release the real coturn allocation before returning from the ignored integration test.
    client.relay(relay_addr)?.close(Instant::now())?;
    send_pending_to_coturn(&mut client, &socket)?;
    let release = receive_from_coturn(&mut client, &socket, Instant::now())?;
    assert_eq!(release.typ.method, METHOD_REFRESH);
    assert_eq!(release.typ.class, CLASS_SUCCESS_RESPONSE);
    assert!(
        !client.relays.contains_key(&relay_addr),
        "coturn confirms allocation release"
    );
    Ok(())
}
