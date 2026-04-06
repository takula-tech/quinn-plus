#[cfg(not(any(target_os = "openbsd", target_os = "netbsd", solarish)))]
use std::net::{SocketAddr, SocketAddrV6};
use std::{
    io::IoSliceMut,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, UdpSocket},
    slice,
};

use quinn_udp::{EcnCodepoint, RecvMeta, Transmit, UdpSocketState};
use socket2::Socket;

const HELLO_PAYLOAD: &[u8] = b"hello";
const GSO_SEGMENT_SIZE: usize = 128;
const SOCKET_BUFFER_SIZE: usize = 123_456;
const ECN_CODEPOINTS: [EcnCodepoint; 2] = [EcnCodepoint::Ect0, EcnCodepoint::Ect1];

#[test]
fn sends_single_datagram_over_loopback() {
    let send_socket = bind_loopback_udp_socket();
    let recv_socket = bind_loopback_udp_socket();
    let destination = recv_socket.local_addr().unwrap();

    test_send_recv(
        &send_socket.into(),
        &recv_socket.into(),
        hello_transmit(destination),
    );
}

#[test]
fn sends_single_datagram_with_explicit_source_ip() {
    let send_socket = bind_loopback_udp_socket();
    let recv_socket = bind_loopback_udp_socket();
    let source_ip = send_socket.local_addr().unwrap().ip();
    let destination = recv_socket.local_addr().unwrap();

    test_send_recv(
        &send_socket.into(),
        &recv_socket.into(),
        hello_transmit_with_src_ip(destination, source_ip),
    );
}

#[test]
fn preserves_ecn_on_ipv6_loopback() {
    let send_socket = bind_ipv6_socket();
    let recv_socket = bind_ipv6_socket();

    assert_ecn_round_trip(&send_socket, &recv_socket, socket_addr(&recv_socket));
}

#[test]
#[cfg(not(any(target_os = "openbsd", target_os = "netbsd", solarish)))]
fn preserves_ecn_on_ipv4_loopback() {
    let send_socket = bind_ipv4_socket();
    let recv_socket = bind_ipv4_socket();

    assert_ecn_round_trip(&send_socket, &recv_socket, socket_addr(&recv_socket));
}

#[test]
#[cfg(not(any(target_os = "openbsd", target_os = "netbsd", solarish)))]
fn preserves_ecn_on_dual_stack_ipv6_socket() {
    let recv_socket = socket2::Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    recv_socket.set_only_v6(false).unwrap();
    // We must use the unspecified address here, rather than a local address, to support dual-stack
    // mode
    recv_socket
        .bind(&socket2::SockAddr::from(
            "[::]:0".parse::<SocketAddr>().unwrap(),
        ))
        .unwrap();
    let recv_v6 = SocketAddr::V6(SocketAddrV6::new(
        Ipv6Addr::LOCALHOST,
        socket_addr(&recv_socket).port(),
        0,
        0,
    ));
    let recv_v4 = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, recv_v6.port()));
    for (source, destination) in [
        (SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0), recv_v6),
        (SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), recv_v4),
    ] {
        let send_socket = Socket::from(UdpSocket::bind(source).unwrap());

        assert_ecn_round_trip(&send_socket, &recv_socket, destination);
    }
}

#[test]
#[cfg(not(any(target_os = "openbsd", target_os = "netbsd", solarish)))]
fn preserves_ecn_on_ipv4_mapped_ipv6_destination() {
    let send_socket = socket2::Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    send_socket.set_only_v6(false).unwrap();
    send_socket
        .bind(&socket2::SockAddr::from(
            "[::]:0".parse::<SocketAddr>().unwrap(),
        ))
        .unwrap();

    let recv_socket = Socket::from(bind_ipv4_udp_socket());
    let recv_v4_mapped_v6 = SocketAddr::V6(SocketAddrV6::new(
        Ipv4Addr::LOCALHOST.to_ipv6_mapped(),
        socket_addr(&recv_socket).port(),
        0,
        0,
    ));

    assert_ecn_round_trip(&send_socket, &recv_socket, recv_v4_mapped_v6);
}

#[test]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows", target_os = "android")),
    ignore
)]
fn sends_gso_batches_over_loopback() {
    let send_socket = bind_loopback_udp_socket();
    let recv_socket = bind_loopback_udp_socket();
    let max_segments = UdpSocketState::new((&send_socket).into())
        .unwrap()
        .max_gso_segments();
    let destination = recv_socket.local_addr().unwrap();
    let payload = vec![0xAB; GSO_SEGMENT_SIZE * max_segments];

    test_send_recv(
        &send_socket.into(),
        &recv_socket.into(),
        gso_transmit(destination, &payload),
    );
}

#[test]
fn configures_socket_buffer_sizes() {
    let send_socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    let recv_socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    for socket in [&send_socket, &recv_socket] {
        bind_ipv4_socket_addr(socket);
        assert_socket_buffer_sizes_can_be_configured(socket);
    }

    test_send_recv(
        &send_socket,
        &recv_socket,
        hello_transmit(socket_addr(&recv_socket)),
    );
}

/// Test Apple fast datapath functionality.
///
/// This test verifies that:
/// 1. `UdpSocketState::new()` auto-enables the fast path when compiled with
///    `fast-apple-datapath`
/// 2. `max_gso_segments()` returns `BATCH_SIZE` after initialization
/// 3. Send/recv works correctly with the fast path enabled
#[test]
#[cfg(apple_fast)]
fn apple_fast_datapath_enables_batching_and_io() {
    let send_socket = bind_ipv4_udp_socket();
    let recv_socket = bind_ipv4_udp_socket();
    let destination = recv_socket.local_addr().unwrap();

    let send_state = UdpSocketState::new((&send_socket).into()).unwrap();
    let recv_state = UdpSocketState::new((&recv_socket).into()).unwrap();

    // Fast path should be enabled automatically on construction
    assert!(
        send_state.is_apple_fast_path_enabled(),
        "fast path should be enabled automatically after new()"
    );
    assert_eq!(
        send_state.max_gso_segments(),
        quinn_udp::BATCH_SIZE,
        "max_gso_segments should be BATCH_SIZE after new()"
    );

    // Verify send/recv still works with fast path enabled
    recv_socket.set_nonblocking(false).unwrap();

    let segments = send_state.max_gso_segments();
    let payload = vec![0xAB; GSO_SEGMENT_SIZE * segments];

    send_state
        .try_send((&send_socket).into(), &gso_transmit(destination, &payload))
        .unwrap();

    // Receive all segments
    let mut receive_buffer = [0u8; u16::MAX as usize];
    let mut total_received = 0;
    while total_received < segments {
        let mut recv_meta = RecvMeta::default();
        let recv_count = recv_state
            .recv(
                (&recv_socket).into(),
                &mut [IoSliceMut::new(&mut receive_buffer)],
                slice::from_mut(&mut recv_meta),
            )
            .unwrap();
        assert_eq!(recv_count, 1);

        let received_segments = recv_meta.len / recv_meta.stride;
        for segment_index in 0..received_segments {
            assert_eq!(
                &receive_buffer
                    [segment_index * recv_meta.stride..(segment_index + 1) * recv_meta.stride],
                &payload[(total_received + segment_index) * GSO_SEGMENT_SIZE
                    ..(total_received + segment_index + 1) * GSO_SEGMENT_SIZE],
                "segment {} content mismatch",
                total_received + segment_index
            );
        }
        total_received += received_segments;
    }
    assert_eq!(total_received, segments, "should receive all segments");
}

fn test_send_recv(send: &Socket, recv: &Socket, transmit: Transmit<'_>) {
    let send_state = UdpSocketState::new(send.into()).unwrap();
    let recv_state = UdpSocketState::new(recv.into()).unwrap();

    // Reverse non-blocking flag set by `UdpSocketState` to make the test non-racy
    recv.set_nonblocking(false).unwrap();

    send_state.try_send(send.into(), &transmit).unwrap();

    let mut receive_buffer = [0; u16::MAX as usize];
    let mut recv_meta = RecvMeta::default();
    let segment_size = transmit.segment_size.unwrap_or(transmit.contents.len());
    let expected_datagrams = transmit.contents.len() / segment_size;
    let mut received_datagrams = 0;

    while received_datagrams < expected_datagrams {
        let recv_count = recv_state
            .recv(
                recv.into(),
                &mut [IoSliceMut::new(&mut receive_buffer)],
                slice::from_mut(&mut recv_meta),
            )
            .unwrap();
        assert_eq!(recv_count, 1);

        let received_segments = recv_meta.len / recv_meta.stride;
        for segment_index in 0..received_segments {
            assert_eq!(
                &receive_buffer
                    [(segment_index * recv_meta.stride)..((segment_index + 1) * recv_meta.stride)],
                &transmit.contents[(received_datagrams + segment_index) * segment_size
                    ..(received_datagrams + segment_index + 1) * segment_size]
            );
        }

        received_datagrams += received_segments;

        assert_eq!(recv_meta.addr.port(), socket_addr(send).port());
        let send_is_ipv6 = socket_addr(send).is_ipv6();
        let recv_is_ipv6 = socket_addr(recv).is_ipv6();
        let mut observed_addresses = vec![recv_meta.addr.ip()];
        if let Some(destination_ip) = recv_meta.dst_ip {
            observed_addresses.push(destination_ip);
        }
        for observed_address in observed_addresses {
            match (send_is_ipv6, recv_is_ipv6) {
                (_, false) => assert_eq!(observed_address, Ipv4Addr::LOCALHOST),
                // Windows gives us real IPv4 addrs, whereas *nix use IPv6-mapped IPv4
                // addrs. Canonicalize to IPv6-mapped for robustness.
                (false, true) => {
                    assert_eq!(
                        ip_to_v6_mapped(observed_address),
                        Ipv4Addr::LOCALHOST.to_ipv6_mapped()
                    )
                }
                (true, true) => assert!(
                    observed_address == Ipv6Addr::LOCALHOST
                        || observed_address == Ipv4Addr::LOCALHOST.to_ipv6_mapped()
                ),
            }
        }

        let ipv4_or_ipv4_mapped_ipv6 = match transmit.destination.ip() {
            IpAddr::V4(_) => true,
            IpAddr::V6(a) => a.to_ipv4_mapped().is_some(),
        };

        // On Android API level <= 25 the IPv4 `IP_TOS` control message is
        // not supported and thus ECN bits can not be received.
        if ipv4_or_ipv4_mapped_ipv6
            && cfg!(target_os = "android")
            && std::env::var("API_LEVEL")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .expect("API_LEVEL environment variable to be set on Android")
                <= 25
        {
            assert_eq!(recv_meta.ecn, None);
        } else {
            assert_eq!(recv_meta.ecn, transmit.ecn);
        }
    }

    assert_eq!(received_datagrams, expected_datagrams);
}

fn ip_to_v6_mapped(x: IpAddr) -> IpAddr {
    match x {
        IpAddr::V4(x) => IpAddr::V6(x.to_ipv6_mapped()),
        IpAddr::V6(_) => x,
    }
}

fn bind_loopback_udp_socket() -> UdpSocket {
    UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
        .or_else(|_| UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)))
        .unwrap()
}

fn bind_ipv4_udp_socket() -> UdpSocket {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap()
}

fn bind_ipv6_socket() -> Socket {
    Socket::from(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap())
}

fn bind_ipv4_socket() -> Socket {
    Socket::from(bind_ipv4_udp_socket())
}

fn bind_ipv4_socket_addr(socket: &Socket) {
    socket
        .bind(&socket2::SockAddr::from(SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            0,
        )))
        .unwrap();
}

fn socket_addr(socket: &Socket) -> SocketAddr {
    socket.local_addr().unwrap().as_socket().unwrap()
}

fn hello_transmit(destination: SocketAddr) -> Transmit<'static> {
    Transmit {
        destination,
        ecn: None,
        contents: HELLO_PAYLOAD,
        segment_size: None,
        src_ip: None,
    }
}

fn hello_transmit_with_src_ip(destination: SocketAddr, src_ip: IpAddr) -> Transmit<'static> {
    Transmit {
        src_ip: Some(src_ip),
        ..hello_transmit(destination)
    }
}

fn ecn_transmit(destination: SocketAddr, codepoint: EcnCodepoint) -> Transmit<'static> {
    Transmit {
        ecn: Some(codepoint),
        ..hello_transmit(destination)
    }
}

fn gso_transmit<'a>(destination: SocketAddr, contents: &'a [u8]) -> Transmit<'a> {
    Transmit {
        destination,
        ecn: None,
        contents,
        segment_size: Some(GSO_SEGMENT_SIZE),
        src_ip: None,
    }
}

fn assert_ecn_round_trip(send: &Socket, recv: &Socket, destination: SocketAddr) {
    for codepoint in ECN_CODEPOINTS {
        test_send_recv(send, recv, ecn_transmit(destination, codepoint));
    }
}

fn assert_socket_buffer_sizes_can_be_configured(socket: &Socket) {
    let factor = socket_buffer_size_factor();
    let socket_state = UdpSocketState::new(socket.into()).expect("created socket state");

    let send_buffer_before = socket_state.send_buffer_size(socket.into()).unwrap();
    assert_ne!(
        send_buffer_before,
        SOCKET_BUFFER_SIZE * factor,
        "make sure buffer is not already desired size"
    );
    socket_state
        .set_send_buffer_size(socket.into(), SOCKET_BUFFER_SIZE)
        .expect("set send buffer size {send_buffer_before} -> {SOCKET_BUFFER_SIZE}");
    let send_buffer_after = socket_state.send_buffer_size(socket.into()).unwrap();
    assert_eq!(
        send_buffer_after,
        SOCKET_BUFFER_SIZE * factor,
        "setting send buffer size to {SOCKET_BUFFER_SIZE} resulted in {send_buffer_before} -> {send_buffer_after}",
    );

    let recv_buffer_before = socket_state.recv_buffer_size(socket.into()).unwrap();
    socket_state
        .set_recv_buffer_size(socket.into(), SOCKET_BUFFER_SIZE)
        .expect("set recv buffer size {recv_buffer_before} -> {SOCKET_BUFFER_SIZE}");
    let recv_buffer_after = socket_state.recv_buffer_size(socket.into()).unwrap();
    assert_eq!(
        recv_buffer_after,
        SOCKET_BUFFER_SIZE * factor,
        "setting recv buffer size to {SOCKET_BUFFER_SIZE} resulted in {recv_buffer_before} -> {recv_buffer_after}",
    );
}

fn socket_buffer_size_factor() -> usize {
    if cfg!(any(target_os = "linux", target_os = "android")) {
        2
    } else {
        1
    }
}
