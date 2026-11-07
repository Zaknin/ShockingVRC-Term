use std::net::Ipv4Addr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalIpv4Interface {
    pub name: String,
    pub address: Ipv4Addr,
    pub kind: &'static str,
    pub requires_review: bool,
}

pub fn is_suitable_local_ipv4(address: Ipv4Addr) -> bool {
    !address.is_unspecified()
        && !address.is_loopback()
        && !address.is_multicast()
        && address != Ipv4Addr::BROADCAST
}

#[cfg(windows)]
pub fn available_local_ipv4() -> Vec<LocalIpv4Interface> {
    use std::{mem::MaybeUninit, os::windows::ffi::OsStringExt};
    use windows_sys::Win32::{
        NetworkManagement::{
            IpHelper::{
                GAA_FLAG_INCLUDE_PREFIX, GetAdaptersAddresses, IF_TYPE_ETHERNET_CSMACD,
                IF_TYPE_IEEE80211, IF_TYPE_SOFTWARE_LOOPBACK, IF_TYPE_TUNNEL,
                IP_ADAPTER_ADDRESSES_LH,
            },
            Ndis::IfOperStatusUp,
        },
        Networking::WinSock::{AF_INET, SOCKADDR_IN},
    };

    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    let mut required = 0_u32;
    // SAFETY: the null output pointer is the documented size-query call.
    let result = unsafe {
        GetAdaptersAddresses(
            AF_INET as u32,
            GAA_FLAG_INCLUDE_PREFIX,
            std::ptr::null(),
            std::ptr::null_mut(),
            &mut required,
        )
    };
    if result != ERROR_BUFFER_OVERFLOW || required == 0 {
        return Vec::new();
    }

    let mut output = Vec::new();
    for _ in 0..3 {
        let units = (required as usize).div_ceil(std::mem::size_of::<u128>());
        let mut storage = Vec::<MaybeUninit<u128>>::with_capacity(units);
        // SAFETY: MaybeUninit<u128> may be left uninitialized until the API fills the buffer.
        unsafe { storage.set_len(units) };
        let mut available = (units * std::mem::size_of::<u128>()) as u32;
        let first = storage.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: the buffer is aligned, writable, and at least `available` bytes long; the API
        // writes linked adapter structures into it and returns pointers within this buffer.
        let result = unsafe {
            GetAdaptersAddresses(
                AF_INET as u32,
                GAA_FLAG_INCLUDE_PREFIX,
                std::ptr::null(),
                first,
                &mut available,
            )
        };
        if result == ERROR_BUFFER_OVERFLOW {
            required = available;
            continue;
        }
        if result != 0 {
            return Vec::new();
        }

        let mut adapter = first;
        while !adapter.is_null() {
            // SAFETY: each link points to a fully initialized adapter record returned by the API.
            let record = unsafe { &*adapter };
            if record.OperStatus == IfOperStatusUp && record.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
                let name = if record.FriendlyName.is_null() {
                    "Network interface".to_owned()
                } else {
                    let mut length = 0;
                    // SAFETY: FriendlyName is a NUL-terminated UTF-16 string owned by the API buffer.
                    unsafe {
                        while *record.FriendlyName.add(length) != 0 { length += 1; }
                        std::ffi::OsString::from_wide(std::slice::from_raw_parts(record.FriendlyName, length))
                    }
                    .to_string_lossy()
                    .into_owned()
                };
                let kind = match record.IfType {
                    IF_TYPE_ETHERNET_CSMACD => "Ethernet",
                    IF_TYPE_IEEE80211 => "Wi-Fi",
                    IF_TYPE_TUNNEL => "Tunnel / VPN",
                    _ => "Other interface",
                };
                let name_lower = name.to_ascii_lowercase();
                let requires_review = matches!(kind, "Tunnel / VPN" | "Other interface")
                    || ["virtual", "vmware", "virtualbox", "hyper-v", "wireguard", "openvpn", "tailscale", "zerotier", "tap-", "vethernet"]
                        .iter()
                        .any(|marker| name_lower.contains(marker));
                let mut address = record.FirstUnicastAddress;
                while !address.is_null() {
                    // SAFETY: each link points to a fully initialized unicast address in the API buffer.
                    let unicast = unsafe { &*address };
                    let socket = unicast.Address.lpSockaddr;
                    if !socket.is_null() {
                        // SAFETY: GetAdaptersAddresses(AF_INET, ...) returns sockaddr_in nodes here.
                        let socket = unsafe { &*(socket.cast::<SOCKADDR_IN>()) };
                        if socket.sin_family == AF_INET {
                            // SAFETY: S_addr is the active 32-bit representation of IN_ADDR.
                            let raw = unsafe { socket.sin_addr.S_un.S_addr };
                            let ip = Ipv4Addr::from(u32::from_be(raw));
                            if is_suitable_local_ipv4(ip) {
                                output.push(LocalIpv4Interface {
                                    name: name.clone(),
                                    address: ip,
                                    kind,
                                    requires_review,
                                });
                            }
                        }
                    }
                    address = unicast.Next;
                }
            }
            adapter = record.Next;
        }
        break;
    }
    output.sort_by_key(|entry| (entry.requires_review, entry.kind, entry.name.clone(), entry.address));
    output.dedup_by(|a, b| a.address == b.address && a.name == b.name);
    output
}

#[cfg(not(windows))]
pub fn available_local_ipv4() -> Vec<LocalIpv4Interface> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_addresses_exclude_unspecified_loopback_and_non_unicast_ipv4() {
        for address in [
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::BROADCAST,
        ] {
            assert!(!is_suitable_local_ipv4(address), "{address} must not be offered");
        }
        assert!(is_suitable_local_ipv4(Ipv4Addr::new(192, 168, 1, 5)));
        assert!(is_suitable_local_ipv4(Ipv4Addr::new(10, 20, 30, 40)));
    }
}
