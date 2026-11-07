use log::{debug, warn};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::ops::Add;
use std::time::{Duration, Instant};
use stun::attributes::*;
use stun::error_code::*;
use stun::fingerprint::*;
use stun::integrity::*;
use stun::message::*;
use stun::textattrs::*;

use super::diagnostics::monotonic_ms;
use super::permission::*;
use super::transaction::*;
use crate::proto;

use crate::client::binding::{BindingManager, BindingState};
use crate::client::{Client, Event, RelayedAddr};
use shared::error::{Error, Result};

const PERM_REFRESH_INTERVAL: Duration = Duration::from_secs(120);
// https://datatracker.ietf.org/doc/html/rfc8656#name-permissions-2
// The Permission Lifetime MUST be 300 seconds (= 5 minutes).
const PERM_LIFETIME: Duration = Duration::from_secs(300);
/// Number of retries after the initial request when a server returns 438.
const MAX_STALE_NONCE_RETRIES: u16 = 3;
/// Smallest step the allocation-refresh timer may advance by.
///
/// The timer advances by a fixed cadence from its own previous value, which keeps the refresh
/// schedule drift-free. That only works while the step is positive. `lifetime` is assigned
/// straight from the server's LIFETIME attribute, and a refresh carrying `LIFETIME=0` — how
/// RFC 5766 deallocates, and what [`Relay::close`] sends — makes `lifetime / 2` zero. The timer
/// then freezes on an already-expired instant and `poll_timeout` reports that same instant
/// forever. A caller that treats an expired deadline as "handle it and go round again" spins
/// on it without ever reaching its socket reads, which is
/// [webrtc#862](https://github.com/webrtc-rs/webrtc/issues/862).
const MIN_ALLOC_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

fn handle_channel_bind_error(binding_mgr: &mut BindingManager, bind_addr: &SocketAddr, err: &Error) {
    if *err == Error::ErrTryAgain {
        // A 438 response only invalidates the nonce. Keep the peer's channel number
        // and retry the bind with the replacement nonce. Deleting this entry makes
        // the next send allocate a different channel number for the same peer, which
        // TURN servers reject.
        if let Some(binding) = binding_mgr.get_by_addr(bind_addr) {
            binding.set_state(BindingState::Idle);
        }
    } else if *err != Error::ErrUnexpectedResponse {
        binding_mgr.delete_by_addr(bind_addr);
    } else if let Some(binding) = binding_mgr.get_by_addr(bind_addr) {
        binding.set_state(BindingState::Failed);
    }
}

fn allocation_refresh_interval(lifetime: Duration, interval_cap: Option<Duration>) -> Duration {
    let half_lifetime = lifetime / 2;
    interval_cap
        .map_or(half_lifetime, |cap| half_lifetime.min(cap))
        .max(MIN_ALLOC_REFRESH_INTERVAL)
}

// RelayState is a set of params use by Relay
pub(crate) struct RelayState {
    pub(crate) relayed_addr: RelayedAddr,
    pub(crate) long_term_integrity_key: Vec<u8>,
    pub(crate) nonce: Nonce,
    /// Monotonic version used to ignore late 438 responses carrying an older nonce.
    nonce_generation: u64,
    pub(crate) lifetime: Duration,
    allocation_refresh_interval_cap: Option<Duration>,
    perm_map: HashMap<SocketAddr, Permission>,
    refresh_alloc_timer: Instant,
    refresh_perms_timer: Instant,
}

impl RelayState {
    pub(super) fn new(
        now: Instant,
        relayed_addr: RelayedAddr,
        long_term_integrity_key: Vec<u8>,
        nonce: Nonce,
        lifetime: Duration,
        allocation_refresh_interval_cap: Option<Duration>,
    ) -> Self {
        debug!("initial lifetime: {} seconds", lifetime.as_secs());

        Self {
            relayed_addr,
            long_term_integrity_key,
            nonce,
            nonce_generation: 0,
            lifetime,
            allocation_refresh_interval_cap,
            perm_map: HashMap::new(),
            refresh_alloc_timer: now.add(allocation_refresh_interval(lifetime, allocation_refresh_interval_cap)),
            refresh_perms_timer: now.add(PERM_REFRESH_INTERVAL),
        }
    }

    /// Accepts a nonce only if this response belongs to the currently installed generation.
    /// Concurrent requests may receive 438 for the same old nonce; a late response must not
    /// roll the allocation back after an earlier response has already advanced it.
    fn update_nonce_from_msg(&mut self, msg: &Message, request_generation: u64) -> Result<u64> {
        if request_generation < self.nonce_generation {
            return Ok(self.nonce_generation);
        }
        if request_generation > self.nonce_generation {
            return Err(Error::Other("TURN nonce generation is inconsistent".to_owned()));
        }

        let nonce = Nonce::get_from_as(msg, ATTR_NONCE)
            .map_err(|_| Error::Other("TURN 438 response did not include a NONCE".to_owned()))?;
        self.nonce = nonce;
        self.nonce_generation = self
            .nonce_generation
            .checked_add(1)
            .ok_or_else(|| Error::Other("TURN nonce generation exhausted".to_owned()))?;
        debug!("TURN stale nonce advanced to generation {}", self.nonce_generation);
        Ok(self.nonce_generation)
    }
}

/// A borrowed handle to one live allocation on the TURN server.
///
/// Obtained from [`Client::relay`](crate::client::Client::relay). Sending to a peer requires
/// a permission for it first — see [`Self::create_permission`].
pub struct Relay<'a> {
    pub(crate) relayed_addr: RelayedAddr,
    pub(crate) client: &'a mut Client,
}

impl Relay<'_> {
    /// Asks the server to permit traffic to and from `peer_addr`.
    ///
    /// The relay silently drops data for peers with no permission, and permissions expire after
    /// five minutes unless refreshed. Returns the transaction id to match against the resulting
    /// [`Event`], or `None` if a permission is already in place.
    ///
    /// # Errors
    ///
    /// Fails if the allocation no longer exists or the request cannot be encoded.
    pub fn create_permission(&mut self, now: Instant, peer_addr: SocketAddr) -> Result<Option<TransactionId>> {
        if let Some(relay) = self.client.relays.get_mut(&self.relayed_addr) {
            relay.perm_map.entry(peer_addr).or_insert_with(Permission::default);
            if let Some(perm) = relay.perm_map.get(&peer_addr)
                && perm.state() == PermState::Idle
            {
                return Ok(Some(self.create_permissions(now, &[peer_addr], Some(peer_addr))?));
            }
            Ok(None)
        } else {
            Err(Error::ErrConnClosed)
        }
    }

    pub(crate) fn poll_timeout(&self) -> Option<Instant> {
        if let Some(relay) = self.client.relays.get(&self.relayed_addr) {
            if relay.refresh_alloc_timer < relay.refresh_perms_timer {
                Some(relay.refresh_alloc_timer)
            } else {
                Some(relay.refresh_perms_timer)
            }
        } else {
            None
        }
    }

    pub(crate) fn handle_timeout(&mut self, now: Instant) {
        let (refresh_alloc_timer, refresh_perms_timer) =
            if let Some(relay) = self.client.relays.get_mut(&self.relayed_addr) {
                let refresh_alloc_timer = if relay.refresh_alloc_timer <= now {
                    // Floored so the timer always moves. A zero step would leave it pinned to an
                    // instant already in the past — see `MIN_ALLOC_REFRESH_INTERVAL`.
                    let step = allocation_refresh_interval(relay.lifetime, relay.allocation_refresh_interval_cap);
                    relay.refresh_alloc_timer = now.add(step);
                    Some(relay.lifetime)
                } else {
                    None
                };

                let refresh_perms_timer = if relay.refresh_perms_timer <= now {
                    relay.refresh_perms_timer = now.add(PERM_REFRESH_INTERVAL);
                    true
                } else {
                    false
                };

                (refresh_alloc_timer, refresh_perms_timer)
            } else {
                (None, false)
            };

        if let Some(lifetime) = refresh_alloc_timer {
            let _ = self.refresh_allocation(now, lifetime);
        }
        if refresh_perms_timer {
            let _ = self.refresh_permissions(now);
        }
    }

    /// Sends `p` to `peer_addr` through the relay.
    ///
    /// Uses ChannelData framing if a channel is bound for that peer, otherwise a Data indication.
    ///
    /// # Errors
    ///
    /// Fails if the allocation is gone, or if no permission exists for `peer_addr`.
    pub fn send_to(&mut self, now: Instant, p: &[u8], peer_addr: SocketAddr) -> Result<()> {
        // check if we have a permission for the destination IP addr
        let (username, realm) = (self.client.username(), self.client.realm());
        let result = if let Some(relay) = self.client.relays.get_mut(&self.relayed_addr) {
            if let Some(perm) = relay.perm_map.get_mut(&peer_addr) {
                if perm.state() != PermState::Permitted {
                    Err(Error::ErrNoPermission)
                } else {
                    Ok((
                        AuthContext {
                            username,
                            realm,
                            integrity_key: relay.long_term_integrity_key.clone(),
                        },
                        relay.nonce.clone(),
                        relay.nonce_generation,
                    ))
                }
            } else {
                Err(Error::ErrNoPermission)
            }
        } else {
            Err(Error::ErrConnClosed)
        };

        let (auth, nonce, nonce_generation) = result?;

        self.send(now, p, peer_addr, auth, nonce, nonce_generation)
    }

    fn send(
        &mut self,
        now: Instant,
        p: &[u8],
        peer_addr: SocketAddr,
        auth: AuthContext,
        nonce: Nonce,
        nonce_generation: u64,
    ) -> Result<()> {
        let channel_number = {
            let (bind_st, bind_at, bind_number, bind_addr) = {
                let b = if let Some(b) = self.client.binding_mgr.find_by_addr(&peer_addr) {
                    b
                } else {
                    self.client
                        .binding_mgr
                        .create(now, peer_addr)
                        .ok_or_else(|| Error::Other("Addr not found".to_owned()))?
                };
                (b.state(), b.refreshed_at(), b.number, b.addr)
            };

            if bind_st == BindingState::Idle || bind_st == BindingState::Request || bind_st == BindingState::Failed {
                // block only callers with the same binding until
                // the binding transaction has been complete
                // binding state may have been changed while waiting. check again.
                if bind_st == BindingState::Idle {
                    if let Some(b) = self.client.binding_mgr.get_by_addr(&bind_addr) {
                        b.set_state(BindingState::Request);
                    }
                    self.channel_bind(
                        now,
                        self.relayed_addr,
                        bind_addr,
                        bind_number,
                        nonce,
                        nonce_generation,
                        auth,
                        0,
                        None,
                    )?;
                }

                // send data using SendIndication
                let mut msg = Message::new();
                msg.build(&[
                    Box::new(TransactionId::new()),
                    Box::new(MessageType::new(METHOD_SEND, CLASS_INDICATION)),
                    Box::new(proto::data::Data(p.to_vec())),
                    Box::new(proto::peeraddr::PeerAddress {
                        ip: peer_addr.ip(),
                        port: peer_addr.port(),
                    }),
                    Box::new(FINGERPRINT),
                ])?;

                // indication has no transaction (fire-and-forget)
                self.client.write_to(now, &msg.raw, self.client.turn_server_addr()?);
                return Ok(());
            }

            // binding is ready
            // check if the binding needs a refresh
            if bind_st == BindingState::Ready && now.saturating_duration_since(bind_at) > PERM_LIFETIME {
                if let Some(b) = self.client.binding_mgr.get_by_addr(&bind_addr) {
                    b.set_state(BindingState::Refresh);
                }
                self.channel_bind(
                    now,
                    self.relayed_addr,
                    bind_addr,
                    bind_number,
                    nonce,
                    nonce_generation,
                    auth,
                    0,
                    None,
                )?;
            }

            bind_number
        };

        // send via ChannelData
        self.send_channel_data(now, p, channel_number)
    }

    // Close closes the connection.
    // Any blocked ReadFrom or write_to operations will be unblocked and return errors.
    /// Releases the allocation by refreshing it with a zero lifetime.
    ///
    /// # Errors
    ///
    /// Fails if the refresh request cannot be sent.
    pub fn close(&mut self, now: Instant) -> Result<()> {
        self.refresh_allocation(now, Duration::from_secs(0))
    }

    fn create_permissions(
        &mut self,
        now: Instant,
        peer_addrs: &[SocketAddr],
        peer_addr_opt: Option<SocketAddr>,
    ) -> Result<TransactionId> {
        let (username, realm) = (self.client.username(), self.client.realm());
        let (auth, nonce, nonce_generation) = if let Some(relay) = self.client.relays.get(&self.relayed_addr) {
            (
                AuthContext {
                    username,
                    realm,
                    integrity_key: relay.long_term_integrity_key.clone(),
                },
                relay.nonce.clone(),
                relay.nonce_generation,
            )
        } else {
            return Err(Error::ErrConnClosed);
        };

        self.queue_create_permissions(now, peer_addrs, peer_addr_opt, auth, nonce, nonce_generation, 0, None)
    }

    fn queue_create_permissions(
        &mut self,
        now: Instant,
        peer_addrs: &[SocketAddr],
        peer_addr_opt: Option<SocketAddr>,
        auth: AuthContext,
        nonce: Nonce,
        nonce_generation: u64,
        retry_count: u16,
        request_id: Option<TransactionId>,
    ) -> Result<TransactionId> {
        let transaction_id = TransactionId::new();
        let msg = {
            let mut setters: Vec<Box<dyn Setter + '_>> = vec![
                Box::new(transaction_id),
                Box::new(MessageType::new(METHOD_CREATE_PERMISSION, CLASS_REQUEST)),
            ];
            for addr in peer_addrs {
                setters.push(Box::new(proto::peeraddr::PeerAddress {
                    ip: addr.ip(),
                    port: addr.port(),
                }));
            }
            setters.push(Box::new(auth.username.clone()));
            setters.push(Box::new(auth.realm.clone()));
            setters.push(Box::new(nonce));
            setters.push(Box::new(MessageIntegrity::new_raw_integrity_with_provider(
                auth.integrity_key.clone(),
                self.client.crypto_provider.crypto(),
            )));
            setters.push(Box::new(FINGERPRINT));

            let mut msg = Message::new();
            msg.build(&setters)?;
            msg
        };
        let request_id = request_id.unwrap_or(transaction_id);
        self.client.perform_transaction_for_request(
            now,
            &msg,
            self.client.turn_server_addr()?,
            TransactionType::CreatePermissionRequest {
                relayed_addr: self.relayed_addr,
                peer_addrs: peer_addrs.to_vec(),
                peer_addr_opt,
                retry_count,
                nonce_generation,
                auth,
            },
            request_id,
        );
        let operation = if peer_addr_opt.is_some() { "create" } else { "refresh" };
        debug!(
            "[rtc-turn] permission {operation} request queued monotonic_ms={} peers={} retry={retry_count}",
            monotonic_ms(),
            peer_addrs.len()
        );
        Ok(request_id)
    }

    pub(super) fn handle_create_permission_response(
        &mut self,
        now: Instant,
        res: Message,
        request_id: TransactionId,
        peer_addrs: Vec<SocketAddr>,
        peer_addr_opt: Option<SocketAddr>,
        retry_count: u16,
        nonce_generation: u64,
        auth: AuthContext,
    ) -> Result<()> {
        if !self.client.relays.contains_key(&self.relayed_addr) {
            return Err(Error::ErrConnClosed);
        }

        if res.typ.class == CLASS_ERROR_RESPONSE {
            let mut code = ErrorCodeAttribute::default();
            let code_result = code.get_from(&res);
            let code_number = code_result
                .as_ref()
                .map_or_else(|_| "unavailable".to_owned(), |()| code.code.0.to_string());
            let err = if code_result.is_err() {
                Error::Other(format!("{}", res.typ))
            } else if code.code == CODE_STALE_NONCE {
                let generation_result = self
                    .client
                    .relays
                    .get_mut(&self.relayed_addr)
                    .expect("allocation checked above")
                    .update_nonce_from_msg(&res, nonce_generation);
                match generation_result {
                    Ok(next_generation) if retry_count < MAX_STALE_NONCE_RETRIES => {
                        let nonce = self.client.relays[&self.relayed_addr].nonce.clone();
                        self.queue_create_permissions(
                            now,
                            &peer_addrs,
                            peer_addr_opt,
                            auth,
                            nonce,
                            next_generation,
                            retry_count + 1,
                            Some(request_id),
                        )?;
                        return Ok(());
                    }
                    Ok(_) => Error::Other(format!(
                        "TURN CreatePermission exceeded {MAX_STALE_NONCE_RETRIES} stale nonce retries"
                    )),
                    Err(err) => err,
                }
            } else {
                Error::Other(format!("{} (error {})", res.typ, code))
            };

            warn!(
                "[rtc-turn] permission {} result monotonic_ms={} result=error error_code={code_number}",
                if peer_addr_opt.is_some() { "create" } else { "refresh" },
                monotonic_ms()
            );
            if let Some(peer_addr) = peer_addr_opt {
                self.client
                    .events
                    .push_back(Event::CreatePermissionError(request_id, err));
                if let Some(relay) = self.client.relays.get_mut(&self.relayed_addr) {
                    relay.perm_map.remove(&peer_addr);
                }
            }
        } else {
            debug!(
                "[rtc-turn] permission {} result monotonic_ms={} result=success",
                if peer_addr_opt.is_some() { "create" } else { "refresh" },
                monotonic_ms()
            );
            if let Some(peer_addr) = peer_addr_opt
                && let Some(relay) = self.client.relays.get_mut(&self.relayed_addr)
                && let Some(perm) = relay.perm_map.get_mut(&peer_addr)
            {
                perm.set_state(PermState::Permitted);
                self.client
                    .events
                    .push_back(Event::CreatePermissionResponse(request_id, peer_addr));
            }
        }

        Ok(())
    }

    pub(super) fn refresh_allocation(&mut self, now: Instant, lifetime: Duration) -> Result<()> {
        let (username, realm) = (self.client.username(), self.client.realm());
        let (auth, nonce, nonce_generation) = if let Some(relay) = self.client.relays.get(&self.relayed_addr) {
            (
                AuthContext {
                    username,
                    realm,
                    integrity_key: relay.long_term_integrity_key.clone(),
                },
                relay.nonce.clone(),
                relay.nonce_generation,
            )
        } else {
            return Err(Error::ErrConnClosed);
        };
        self.queue_refresh_allocation(now, lifetime, auth, nonce, nonce_generation, 0, None)?;
        Ok(())
    }

    fn queue_refresh_allocation(
        &mut self,
        now: Instant,
        lifetime: Duration,
        auth: AuthContext,
        nonce: Nonce,
        nonce_generation: u64,
        retry_count: u16,
        request_id: Option<TransactionId>,
    ) -> Result<TransactionId> {
        let transaction_id = TransactionId::new();
        let mut msg = Message::new();
        msg.build(&[
            Box::new(transaction_id),
            Box::new(MessageType::new(METHOD_REFRESH, CLASS_REQUEST)),
            Box::new(proto::lifetime::Lifetime(lifetime)),
            Box::new(auth.username.clone()),
            Box::new(auth.realm.clone()),
            Box::new(nonce),
            Box::new(MessageIntegrity::new_raw_integrity_with_provider(
                auth.integrity_key.clone(),
                self.client.crypto_provider.crypto(),
            )),
            Box::new(FINGERPRINT),
        ])?;
        let request_id = request_id.unwrap_or(transaction_id);
        self.client.perform_transaction_for_request(
            now,
            &msg,
            self.client.turn_server_addr()?,
            TransactionType::RefreshRequest {
                relayed_addr: self.relayed_addr,
                lifetime,
                retry_count,
                nonce_generation,
                auth,
            },
            request_id,
        );
        debug!(
            "[rtc-turn] allocation Refresh request queued monotonic_ms={} retry={retry_count}",
            monotonic_ms()
        );
        Ok(request_id)
    }

    pub(super) fn handle_refresh_allocation_response(
        &mut self,
        now: Instant,
        res: Message,
        request_id: TransactionId,
        lifetime: Duration,
        retry_count: u16,
        nonce_generation: u64,
        auth: AuthContext,
    ) -> Result<()> {
        if !self.client.relays.contains_key(&self.relayed_addr) {
            return Err(Error::ErrConnClosed);
        }

        if res.typ.class == CLASS_ERROR_RESPONSE {
            let mut code = ErrorCodeAttribute::default();
            code.get_from(&res).map_err(|_| Error::Other(format!("{}", res.typ)))?;
            if code.code == CODE_STALE_NONCE {
                let next_generation = self
                    .client
                    .relays
                    .get_mut(&self.relayed_addr)
                    .expect("allocation checked above")
                    .update_nonce_from_msg(&res, nonce_generation)?;
                if retry_count >= MAX_STALE_NONCE_RETRIES {
                    warn!(
                        "[rtc-turn] allocation Refresh result monotonic_ms={} result=error error_code=438 retries_exhausted=true",
                        monotonic_ms()
                    );
                    return Err(Error::Other(format!(
                        "TURN Refresh exceeded {MAX_STALE_NONCE_RETRIES} stale nonce retries"
                    )));
                }
                let nonce = self.client.relays[&self.relayed_addr].nonce.clone();
                self.queue_refresh_allocation(
                    now,
                    lifetime,
                    auth,
                    nonce,
                    next_generation,
                    retry_count + 1,
                    Some(request_id),
                )?;
                warn!(
                    "[rtc-turn] allocation Refresh stale nonce retried immediately monotonic_ms={} retry={}",
                    monotonic_ms(),
                    retry_count + 1
                );
                return Ok(());
            }

            let error = Error::Other(format!("{} (error {})", res.typ, code));
            if code.code.0 == 437 {
                self.client.relays.remove(&self.relayed_addr);
                warn!(
                    "[rtc-turn] allocation Refresh found no allocation monotonic_ms={} error_code=437",
                    monotonic_ms()
                );
            } else {
                warn!(
                    "[rtc-turn] allocation Refresh result monotonic_ms={} result=error error_code={}",
                    monotonic_ms(),
                    code.code.0
                );
            }
            return Err(error);
        }

        let relay = self
            .client
            .relays
            .get_mut(&self.relayed_addr)
            .ok_or(Error::ErrConnClosed)?;
        let mut updated_lifetime = proto::lifetime::Lifetime::default();
        updated_lifetime.get_from(&res)?;
        relay.lifetime = updated_lifetime.0;
        relay.refresh_alloc_timer = now.add(allocation_refresh_interval(
            relay.lifetime,
            relay.allocation_refresh_interval_cap,
        ));
        debug!("updated lifetime: {} seconds", relay.lifetime.as_secs());
        debug!(
            "[rtc-turn] allocation Refresh result monotonic_ms={} result=success lifetime_seconds={}",
            monotonic_ms(),
            relay.lifetime.as_secs()
        );
        let released = relay.lifetime.is_zero();
        if released {
            self.client.relays.remove(&self.relayed_addr);
            debug!("allocation released; relay {} dropped", self.relayed_addr);
        }
        Ok(())
    }

    pub(super) fn refresh_permissions(&mut self, now: Instant) -> Result<()> {
        if let Some(relay) = self.client.relays.get_mut(&self.relayed_addr) {
            #[allow(clippy::map_clone)]
            let addrs: Vec<SocketAddr> = relay.perm_map.keys().map(|addr| *addr).collect();
            if addrs.is_empty() {
                debug!("no permission to refresh");
                return Ok(());
            }
            let _ = self.create_permissions(now, &addrs, None)?;
            Ok(())
        } else {
            Err(Error::ErrConnClosed)
        }
    }

    fn channel_bind(
        &mut self,
        now: Instant,
        relayed_addr: RelayedAddr,
        bind_addr: SocketAddr,
        bind_number: u16,
        nonce: Nonce,
        nonce_generation: u64,
        auth: AuthContext,
        retry_count: u16,
        request_id: Option<TransactionId>,
    ) -> Result<()> {
        let (msg, turn_server_addr) = {
            let setters: Vec<Box<dyn Setter + '_>> = vec![
                Box::new(TransactionId::new()),
                Box::new(MessageType::new(METHOD_CHANNEL_BIND, CLASS_REQUEST)),
                Box::new(proto::peeraddr::PeerAddress {
                    ip: bind_addr.ip(),
                    port: bind_addr.port(),
                }),
                Box::new(proto::channum::ChannelNumber(bind_number)),
                Box::new(auth.username.clone()),
                Box::new(auth.realm.clone()),
                Box::new(nonce),
                // Built here rather than by the caller: it borrows `crypto_provider`, and that
                // borrow must end with `setters` so the `&mut self` transaction below is free.
                Box::new(MessageIntegrity::new_raw_integrity_with_provider(
                    auth.integrity_key.clone(),
                    self.client.crypto_provider.crypto(),
                )),
                Box::new(FINGERPRINT),
            ];

            let mut msg = Message::new();
            msg.build(&setters)?;

            (msg, self.client.turn_server_addr()?)
        };

        debug!("[rtc-turn] ChannelBind request queued monotonic_ms={}", monotonic_ms());
        let request_id = request_id.unwrap_or(msg.transaction_id);
        let _ = self.client.perform_transaction_for_request(
            now,
            &msg,
            turn_server_addr,
            TransactionType::ChannelBindRequest {
                relayed_addr,
                bind_addr,
                bind_number,
                retry_count,
                nonce_generation,
                auth,
            },
            request_id,
        );

        Ok(())
    }

    pub(super) fn handle_channel_bind_response(
        &mut self,
        now: Instant,
        res: Message,
        request_id: TransactionId,
        bind_addr: SocketAddr,
        bind_number: u16,
        retry_count: u16,
        nonce_generation: u64,
        auth: AuthContext,
    ) -> Result<()> {
        if !self.client.relays.contains_key(&self.relayed_addr) {
            return Err(Error::ErrConnClosed);
        }

        if res.typ.class == CLASS_ERROR_RESPONSE {
            let mut code = ErrorCodeAttribute::default();
            if code.get_from(&res).is_err() {
                let error = Error::Other(format!("{}", res.typ));
                handle_channel_bind_error(&mut self.client.binding_mgr, &bind_addr, &error);
                self.client
                    .events
                    .push_back(Event::ChannelBindError(request_id, bind_addr, error));
                warn!(
                    "[rtc-turn] ChannelBind result monotonic_ms={} result=error error_code=unavailable",
                    monotonic_ms()
                );
                return Ok(());
            }

            if code.code == CODE_STALE_NONCE {
                let update = self
                    .client
                    .relays
                    .get_mut(&self.relayed_addr)
                    .expect("allocation checked above")
                    .update_nonce_from_msg(&res, nonce_generation);
                match update {
                    Ok(next_generation) if retry_count < MAX_STALE_NONCE_RETRIES => {
                        let nonce = self.client.relays[&self.relayed_addr].nonce.clone();
                        self.channel_bind(
                            now,
                            self.relayed_addr,
                            bind_addr,
                            bind_number,
                            nonce,
                            next_generation,
                            auth,
                            retry_count + 1,
                            Some(request_id),
                        )?;
                        warn!(
                            "[rtc-turn] ChannelBind stale nonce retried immediately monotonic_ms={} retry={}",
                            monotonic_ms(),
                            retry_count + 1
                        );
                        return Ok(());
                    }
                    Ok(_) => {
                        warn!("[rtc-turn] ChannelBind stopped after {MAX_STALE_NONCE_RETRIES} stale nonce retries")
                    }
                    Err(error) => warn!("[rtc-turn] ChannelBind stale nonce could not be applied: {error}"),
                }
                // Preserve the corrected channel-number mapping on exhaustion as well. A later
                // send can try again using the newest nonce without inventing a new number.
                handle_channel_bind_error(&mut self.client.binding_mgr, &bind_addr, &Error::ErrTryAgain);
                self.client.events.push_back(Event::ChannelBindError(
                    request_id,
                    bind_addr,
                    Error::Other(format!(
                        "TURN ChannelBind did not recover after {MAX_STALE_NONCE_RETRIES} stale nonce retries"
                    )),
                ));
                return Ok(());
            }

            let error = Error::Other(format!("{} (error {})", res.typ, code));
            handle_channel_bind_error(&mut self.client.binding_mgr, &bind_addr, &error);
            self.client
                .events
                .push_back(Event::ChannelBindError(request_id, bind_addr, error));
            warn!(
                "[rtc-turn] ChannelBind result monotonic_ms={} result=error error_code={}",
                monotonic_ms(),
                code.code.0
            );
            Ok(())
        } else if res.typ != MessageType::new(METHOD_CHANNEL_BIND, CLASS_SUCCESS_RESPONSE) {
            let error = Error::ErrUnexpectedResponse;
            handle_channel_bind_error(&mut self.client.binding_mgr, &bind_addr, &error);
            self.client
                .events
                .push_back(Event::ChannelBindError(request_id, bind_addr, error));
            Ok(())
        } else {
            if let Some(b) = self.client.binding_mgr.get_by_addr(&bind_addr) {
                b.set_refreshed_at(now);
                b.set_state(BindingState::Ready);
            }
            debug!("[rtc-turn] ChannelBind result monotonic_ms={} result=success", monotonic_ms());
            Ok(())
        }
    }

    pub(super) fn handle_channel_bind_timeout(&mut self, request_id: TransactionId, bind_addr: SocketAddr) {
        // A timeout leaves server-side state uncertain, but the binding can safely be requested
        // again later. Keep its channel number so a repeated bind for this peer is stable.
        handle_channel_bind_error(&mut self.client.binding_mgr, &bind_addr, &Error::ErrTryAgain);
        self.client.events.push_back(Event::ChannelBindError(
            request_id,
            bind_addr,
            Error::Other("TURN ChannelBind transaction timed out".to_owned()),
        ));
    }

    fn send_channel_data(&mut self, now: Instant, data: &[u8], channel_number: u16) -> Result<()> {
        let mut ch_data = proto::chandata::ChannelData {
            data: data.to_vec(),
            number: proto::channum::ChannelNumber(channel_number),
            ..Default::default()
        };
        ch_data.encode();

        self.client.write_to(now, &ch_data.raw, self.client.turn_server_addr()?);

        Ok(())
    }
}

#[cfg(test)]
mod channel_bind_retry_tests {
    use super::*;
    use crate::client::binding::BindingManager;

    #[test]
    fn stale_nonce_retries_same_peer_with_same_channel_number() {
        let now = Instant::now();
        let peer = "192.0.2.10:50000".parse().expect("peer address");
        let mut bindings = BindingManager::new();
        let first_number = bindings.create(now, peer).expect("binding").number;
        bindings
            .get_by_addr(&peer)
            .expect("binding")
            .set_state(BindingState::Refresh);

        handle_channel_bind_error(&mut bindings, &peer, &Error::ErrTryAgain);

        let retry = bindings.find_by_addr(&peer).expect("retained binding");
        assert_eq!(retry.number, first_number);
        assert_eq!(retry.state(), BindingState::Idle);
    }
}
