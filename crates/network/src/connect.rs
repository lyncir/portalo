use std::{future::Future, net::SocketAddr, time::Duration};
use tokio::{net::TcpStream, time::timeout};

// PORTALO1 has no receiver-ready handshake. Concurrent connects to different
// interfaces of the same peer can consume its receive slots, so try serially.
pub(super) async fn connect(addresses: Vec<SocketAddr>) -> anyhow::Result<(TcpStream, SocketAddr)> {
    sequential(addresses, |address| async move {
        TcpStream::connect(address).await
    })
    .await
}

async fn sequential<T, F, Fut>(
    addresses: Vec<SocketAddr>,
    connect: F,
) -> anyhow::Result<(T, SocketAddr)>
where
    F: Fn(SocketAddr) -> Fut,
    Fut: Future<Output = std::io::Result<T>>,
{
    timeout(Duration::from_secs(10), async {
        let mut seen = std::collections::HashSet::new();
        for address in addresses {
            if !seen.insert(address) {
                continue;
            }
            if let Ok(Ok(socket)) = timeout(Duration::from_secs(2), connect(address)).await {
                return Ok((socket, address));
            }
        }
        anyhow::bail!("No reachable device address")
    })
    .await
    .map_err(|_| anyhow::anyhow!("Connection deadline exceeded"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Attempt(Arc<AtomicUsize>);
    impl Drop for Attempt {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_drops_attempt_before_next_and_duplicates_are_skipped() {
        let first = SocketAddr::from(([127, 0, 0, 1], 1));
        let second = SocketAddr::from(([127, 0, 0, 1], 2));
        let active = Arc::new(AtomicUsize::new(0));
        let calls = AtomicUsize::new(0);
        let start = tokio::time::Instant::now();
        let (_, address) = sequential(vec![first, first, second], |address| {
            calls.fetch_add(1, Ordering::SeqCst);
            let active = active.clone();
            async move {
                assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                let _attempt = Attempt(active);
                if address == first {
                    std::future::pending::<()>().await;
                }
                Ok(())
            }
        })
        .await
        .unwrap();
        assert_eq!(address, second);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn total_deadline_is_bounded() {
        let addresses = (1..100)
            .map(|port| SocketAddr::from(([127, 0, 0, 1], port)))
            .collect();
        let start = tokio::time::Instant::now();
        let error = sequential(addresses, |_| std::future::pending::<std::io::Result<()>>())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("deadline"));
        assert_eq!(start.elapsed(), Duration::from_secs(10));
    }
}
