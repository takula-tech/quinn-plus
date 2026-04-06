//! Criterion benchmark: fixed-volume UDP echo between two `UdpSocketState` endpoints.
//! Run `cargo bench -p quinn-udp --features fast-apple-datapath` from project root dir
//!
//! For each supported combination of GSO, GRO, and batched receive (`recvmmsg`), measures how
//! long it takes to send `TOTAL_BYTES` and drain the receiver until caught up.

use std::{
    cmp::min,
    io::{ErrorKind, IoSliceMut},
    net::{Ipv4Addr, Ipv6Addr, UdpSocket},
};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use tokio::{io::Interest, runtime::Runtime};

use quinn_udp::{BATCH_SIZE, RecvMeta, Transmit, UdpSocketState};

// --- Protocol / workload constants ---

const TOTAL_BYTES: usize = 10 * 1024 * 1024;
const SEGMENT_SIZE: usize = 1280;

const MAX_IP_UDP_HEADER_SIZE: usize = 48;
const MAX_DATAGRAM_SIZE: usize = u16::MAX as usize - MAX_IP_UDP_HEADER_SIZE;

// --- Scenario matrix (what Criterion reports as separate groups) ---

/// One row in the benchmark matrix: Linux UDP offload and batching flags.
#[derive(Clone, Copy)]
struct SocketFeatureSet {
    gso_enabled: bool,
    gro_enabled: bool,
    recvmmsg_enabled: bool,
}

impl SocketFeatureSet {
    /// Stable Criterion group id (historical comparisons rely on this shape).
    fn criterion_group_name(self) -> String {
        format!(
            "gso_{}_gro_{}_recvmmsg_{}",
            self.gso_enabled, self.gro_enabled, self.recvmmsg_enabled
        )
    }
}

/// Every `(GSO, GRO, recvmmsg)` combination exercised on this target.
///
/// On Windows, GSO without GRO is omitted: without GRO the receive path must accept a full GSO-sized
/// datagram, which this bench does not size buffers for. (See OS error on undersized recv buffer.)
fn all_socket_feature_sets() -> Vec<SocketFeatureSet> {
    let mut scenarios = Vec::new();

    for gso_enabled in [
        false,
        #[cfg(any(target_os = "linux", target_os = "windows", apple))]
        true,
    ] {
        for gro_enabled in [false, true] {
            #[cfg(target_os = "windows")]
            if gso_enabled && !gro_enabled {
                continue;
            }

            for recvmmsg_enabled in [false, true] {
                scenarios.push(SocketFeatureSet {
                    gso_enabled,
                    gro_enabled,
                    recvmmsg_enabled,
                });
            }
        }
    }

    scenarios
}

pub fn criterion_benchmark(c: &mut Criterion) {
    // Arrange: shared async runtime and socket pair
    let runtime = Runtime::new().unwrap();
    let _runtime_guard = runtime.enter();

    let (sender_state, sender_socket) = new_bound_socket_pair();
    let (receiver_state, receiver_socket) = new_bound_socket_pair();
    let receiver_addr = receiver_socket.local_addr().unwrap();

    for scenario in all_socket_feature_sets() {
        // Arrange: Criterion group + reported throughput denominator (bytes per iteration)
        let mut group = c.benchmark_group(scenario.criterion_group_name());
        group.throughput(Throughput::Bytes(TOTAL_BYTES as u64));

        let gso_segment_count = if scenario.gso_enabled {
            sender_state.max_gso_segments()
        } else {
            1
        };
        let send_payload_len = min(MAX_DATAGRAM_SIZE, SEGMENT_SIZE * gso_segment_count);
        let send_payload = vec![0xAB; send_payload_len];

        let transmit = Transmit {
            destination: receiver_addr,
            ecn: None,
            contents: &send_payload,
            segment_size: scenario.gso_enabled.then_some(SEGMENT_SIZE),
            src_ip: None,
        };

        let gro_segment_count = if scenario.gro_enabled {
            receiver_state.gro_segments()
        } else {
            1
        };
        let recv_batch_len = if scenario.recvmmsg_enabled {
            BATCH_SIZE
        } else {
            1
        };

        // Act: time one full quota send/recv cycle (no assertions; Criterion measures duration)
        group.bench_function("throughput", |bencher| {
            bencher.to_async(&runtime).iter(|| {
                send_until_byte_quota(
                    &sender_state,
                    &sender_socket,
                    &receiver_state,
                    &receiver_socket,
                    &transmit,
                    recv_batch_len,
                    gro_segment_count,
                )
            })
        });
    }
}

/// Sends until `TOTAL_BYTES` have been submitted, receiving after each send until the receiver
/// has caught up.
async fn send_until_byte_quota(
    sender_state: &UdpSocketState,
    sender_socket: &tokio::net::UdpSocket,
    receiver_state: &UdpSocketState,
    receiver_socket: &tokio::net::UdpSocket,
    transmit: &Transmit<'_>,
    recv_batch_len: usize,
    gro_segment_count: usize,
) {
    let mut recv_buffers = vec![vec![0u8; SEGMENT_SIZE * gro_segment_count]; recv_batch_len];
    let mut recv_slices: Vec<IoSliceMut<'_>> = recv_buffers
        .iter_mut()
        .map(|buf| IoSliceMut::new(buf))
        .collect();
    let mut recv_meta = vec![RecvMeta::default(); recv_batch_len];

    let mut bytes_sent: usize = 0;
    let mut bytes_received: usize = 0;

    while bytes_sent < TOTAL_BYTES {
        sender_socket.writable().await.unwrap();
        sender_socket
            .try_io(Interest::WRITABLE, || {
                sender_state.send((&sender_socket).into(), transmit)
            })
            .unwrap();
        bytes_sent += transmit.contents.len();

        while bytes_received < bytes_sent {
            receiver_socket.readable().await.unwrap();
            let datagrams_in_batch = match receiver_socket.try_io(Interest::READABLE, || {
                receiver_state.recv((&receiver_socket).into(), &mut recv_slices, &mut recv_meta)
            }) {
                Ok(n) => n,
                // `readable()` can wake spuriously; retry the recv.
                Err(err) if err.kind() == ErrorKind::WouldBlock => continue,
                other => other.unwrap(),
            };
            bytes_received += recv_meta
                .iter()
                .map(|m| m.len)
                .take(datagrams_in_batch)
                .sum::<usize>();
        }
    }
}

/// Std UDP socket bound to localhost plus matching `UdpSocketState`.
fn new_bound_socket_pair() -> (UdpSocketState, tokio::net::UdpSocket) {
    let std_socket = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
        .or_else(|_| UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)))
        .unwrap();

    let state = UdpSocketState::new((&std_socket).into()).unwrap();

    let async_socket = tokio::net::UdpSocket::from_std(std_socket).unwrap();

    (state, async_socket)
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
