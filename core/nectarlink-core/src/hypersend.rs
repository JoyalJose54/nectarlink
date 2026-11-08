// SPDX-License-Identifier: MPL-2.0
//! HyperSend: raw-TCP fast path for file transfers over mobile hotspot LANs.
//!
//! Mobile hotspot links (e.g. phone tethering a PC over 5 GHz Wi-Fi) have
//! very low latency (<2 ms) and near-zero packet loss, but QUIC's userspace
//! congestion control and encryption overhead limit transfer throughput.
//!
//! HyperSend establishes a direct, single-stream raw TCP channel for bulk
//! file transfer while retaining QUIC for authentication, metadata, and control.
//!
//! Key invariants:
//! 1. Strictly gated to mobile hotspot subnets — never used on regular home Wi-Fi.
//! 2. 100% transparent: any connection failure, timeout, or error silently falls
//!    back to standard QUIC with zero user-visible error and no cancelled transfers.
//! 3. Calls `reporter.progress()` on every chunk so the watchdog and UI stay alive.
//! 4. Clean two-way confirmation: receiver acknowledges full completion with `0xAC`.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use nectarlink_protocol::messages::{FileEntry, FilesOffer};
use crate::transfer::Reporter;
use crate::session::Session;

/// Magic header sent by the connecting sender to verify the HyperSend channel.
pub const HYPERSEND_MAGIC: u32 = 0x48595045; // "HYPE"
/// Confirmation byte sent by receiver when all bytes are safely on disk.
pub const HYPERSEND_CONFIRM: u8 = 0xAC;

/// Timeout waiting for the incoming TCP connection on receiver side.
pub const ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout for connecting to receiver on sender side.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Chunk size for TCP read/write buffer (512 KB).
pub const CHUNK_SIZE: usize = 512 * 1024;
/// Progress report interval in bytes (every 256 KB).
pub const PROGRESS_INTERVAL: u64 = 256 * 1024;

/// Known mobile hotspot subnet prefixes.
const HOTSPOT_SUBNETS: &[([u8; 3], u8)] = &[
    ([10, 15, 70], 24),  // Samsung Galaxy default hotspot
    ([10, 15, 71], 24),  // Samsung Galaxy hotspot alternate
    ([10, 15, 72], 24),  // Samsung Galaxy hotspot alternate
    ([10, 15, 73], 24),  // Samsung Galaxy hotspot alternate
    ([10, 0, 3], 24),    // Samsung Wi-Fi sharing / tether
    ([192, 168, 43], 24), // Android standard portable hotspot
    ([192, 168, 49], 24), // Android Wi-Fi Direct / tethering
    ([172, 20, 10], 24),  // iOS personal hotspot
];

/// Returns `true` if the IP address belongs to a known mobile hotspot subnet.
pub fn is_hotspot_ip(addr: IpAddr) -> bool {
    let IpAddr::V4(v4) = addr else { return false };
    let o = v4.octets();
    // 10.15.x.x is almost universally a Samsung hotspot tethering subnet
    if o[0] == 10 && o[1] == 15 {
        return true;
    }
    for &([a, b, c], prefix) in HOTSPOT_SUBNETS {
        if prefix == 24 && o[0] == a && o[1] == b && o[2] == c {
            return true;
        }
    }
    false
}

/// Returns `true` if any local non-loopback IPv4 address matches a hotspot pattern.
pub fn is_on_mobile_hotspot() -> bool {
    let probes = [
        "10.15.70.1:1",
        "10.0.3.1:1",
        "192.168.43.1:1",
        "192.168.49.1:1",
        "172.20.10.1:1",
    ];
    for probe in probes {
        if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
            if sock.connect(probe).is_ok() {
                if let Ok(local) = sock.local_addr() {
                    if is_hotspot_ip(local.ip()) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Determines if the current session with a peer is on a mobile hotspot.
pub fn is_hotspot_session(session: &Session) -> bool {
    if session.path() != crate::events::ConnectionPath::Lan {
        return false;
    }
    for addr in session.remote_ip_addrs() {
        if is_hotspot_ip(addr.ip()) {
            return true;
        }
    }
    for p in &session.conn.paths() {
        if let iroh::TransportAddr::Ip(sock) = p.remote_addr() {
            if is_hotspot_ip(sock.ip()) {
                return true;
            }
        }
    }
    is_on_mobile_hotspot()
}

/// Resolves the primary direct IP address of the peer for HyperSend TCP connection.
pub fn peer_direct_ip(session: &Session) -> Option<IpAddr> {
    let paths = session.conn.paths();
    if let Some(p) = paths.iter().find(|p| p.is_selected()) {
        if let iroh::TransportAddr::Ip(sock) = p.remote_addr() {
            if !sock.ip().is_loopback() {
                return Some(sock.ip());
            }
        }
    }
    for addr in session.remote_ip_addrs() {
        if !addr.ip().is_loopback() {
            return Some(addr.ip());
        }
    }
    None
}

/// HyperSend sender engine.
/// Sends all remaining file bytes sequentially over a single direct TCP stream.
pub async fn send_hypersend(
    reporter: &mut Reporter,
    peer_addr: SocketAddr,
    files: &mut [(FileEntry, tokio::fs::File)],
    have: &[u64],
    cancel: &CancellationToken,
    pause_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    tracing::info!(%peer_addr, "HyperSend: connecting direct TCP stream");

    let connect_fut = TcpStream::connect(peer_addr);
    let mut stream = tokio::select! {
        _ = cancel.cancelled() => return Err("cancelled".into()),
        res = tokio::time::timeout(CONNECT_TIMEOUT, connect_fut) => match res {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(format!("TCP connect error: {e}")),
            Err(_) => return Err("TCP connect timed out".into()),
        },
    };

    let _ = stream.set_nodelay(true);

    // Send magic header
    stream.write_u32_le(HYPERSEND_MAGIC).await.map_err(|e| format!("magic write error: {e}"))?;

    let mut done: u64 = files.iter().zip(have).map(|((entry, _), h)| (*h).min(entry.size)).sum();
    let mut last_progress_report = done;

    let mut buf = vec![0u8; CHUNK_SIZE];

    for ((entry, file), &already_have) in files.iter_mut().zip(have) {
        let start = already_have.min(entry.size);
        let mut left = entry.size - start;

        // Write remaining length for this file
        stream.write_u64_le(left).await.map_err(|e| format!("len write error: {e}"))?;

        if left == 0 {
            continue;
        }

        file.seek(std::io::SeekFrom::Start(start)).await.map_err(|e| format!("seek error: {e}"))?;

        while left > 0 {
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            if *pause_rx.borrow_and_update() {
                return Err("paused".into());
            }

            let to_read = (buf.len() as u64).min(left) as usize;
            let n = file.read_exact(&mut buf[..to_read]).await.map_err(|e| format!("read error: {e}"))?;
            stream.write_all(&buf[..to_read]).await.map_err(|e| format!("write error: {e}"))?;

            left -= n as u64;
            done += n as u64;

            if done.saturating_sub(last_progress_report) >= PROGRESS_INTERVAL || left == 0 {
                reporter.progress(done);
                last_progress_report = done;
            }
        }
    }

    stream.flush().await.map_err(|e| format!("flush error: {e}"))?;

    // Wait for receiver confirmation byte
    let confirm = tokio::time::timeout(Duration::from_secs(15), stream.read_u8())
        .await
        .map_err(|_| "confirmation timeout".to_string())?
        .map_err(|e| format!("confirmation read error: {e}"))?;

    if confirm != HYPERSEND_CONFIRM {
        return Err(format!("bad confirmation: 0x{confirm:02x}"));
    }

    reporter.progress(done);
    tracing::info!(bytes = done, "HyperSend: send completed successfully");
    Ok(())
}

/// HyperSend receiver engine.
/// Accepts the incoming TCP stream and writes remaining bytes to .part files.
pub async fn receive_hypersend(
    reporter: &mut Reporter,
    listener: TcpListener,
    offer: &FilesOffer,
    have: &[u64],
    part: &impl Fn(usize) -> std::path::PathBuf,
    cancel: &CancellationToken,
    pause_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    tracing::info!("HyperSend: receiver waiting for incoming TCP stream");

    let accept_fut = listener.accept();
    let (mut stream, remote_addr) = tokio::select! {
        _ = cancel.cancelled() => return Err("cancelled".into()),
        res = tokio::time::timeout(ACCEPT_TIMEOUT, accept_fut) => match res {
            Ok(Ok((s, addr))) => (s, addr),
            Ok(Err(e)) => return Err(format!("TCP accept error: {e}")),
            Err(_) => return Err("TCP accept timed out".into()),
        },
    };

    let _ = stream.set_nodelay(true);
    tracing::info!(%remote_addr, "HyperSend: TCP stream connected");

    // Read and verify magic header
    let magic = stream.read_u32_le().await.map_err(|e| format!("magic read error: {e}"))?;
    if magic != HYPERSEND_MAGIC {
        return Err(format!("invalid magic header: 0x{magic:08x}"));
    }

    let mut done: u64 = have.iter().sum();
    let mut last_progress_report = done;
    let mut buf = vec![0u8; CHUNK_SIZE];

    for (i, (file, &already_have)) in offer.files.iter().zip(have).enumerate() {
        let expected_left = file.size.saturating_sub(already_have);
        let left_from_sender = stream.read_u64_le().await.map_err(|e| format!("len read error: {e}"))?;

        if left_from_sender != expected_left {
            return Err(format!("length mismatch: expected {expected_left}, got {left_from_sender}"));
        }

        if expected_left == 0 {
            continue;
        }

        let part_path = part(i);
        let mut out = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&part_path)
            .await
            .map_err(|e| format!("part file open error: {e}"))?;

        if already_have > 0 {
            out.seek(std::io::SeekFrom::Start(already_have)).await.map_err(|e| format!("seek error: {e}"))?;
        }

        let mut left = expected_left;
        while left > 0 {
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            if *pause_rx.borrow_and_update() {
                return Err("paused".into());
            }

            let to_read = (buf.len() as u64).min(left) as usize;
            stream.read_exact(&mut buf[..to_read]).await.map_err(|e| format!("read error: {e}"))?;
            out.write_all(&buf[..to_read]).await.map_err(|e| format!("write error: {e}"))?;

            left -= to_read as u64;
            done += to_read as u64;

            if done.saturating_sub(last_progress_report) >= PROGRESS_INTERVAL || left == 0 {
                reporter.progress(done);
                last_progress_report = done;
            }
        }
        out.flush().await.map_err(|e| format!("part flush error: {e}"))?;
    }

    // Send confirmation byte
    stream.write_u8(HYPERSEND_CONFIRM).await.map_err(|e| format!("confirm write error: {e}"))?;
    stream.flush().await.map_err(|e| format!("confirm flush error: {e}"))?;

    reporter.progress(done);
    tracing::info!(bytes = done, "HyperSend: receive completed successfully");
    Ok(())
}
