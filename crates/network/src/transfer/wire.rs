use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(super) const IDLE: Duration = Duration::from_secs(120);
pub(super) const MAX_META: usize = 16 * 1024 * 1024;
// Two blocks per stream keep payload credit at 64 MiB across two four-stream
// receive tasks. Metadata, transient frame copies and UI are separate budgets.
pub(super) const WINDOW: usize = 2 * super::BLOCK;
fn legacy_window() -> usize {
    super::BLOCK
}

#[derive(Serialize, Deserialize, Debug)]
pub(super) enum Message {
    Hello {
        version: u32,
    },
    Ready {
        streams: usize,
        frame: usize,
        block: usize,
        #[serde(default = "legacy_window")]
        window: usize,
        #[serde(default)]
        streaming: bool,
    },
    Begin {
        manifest: super::Manifest,
        token: String,
    },
    Resume {
        offsets: Vec<u64>,
        result: Option<String>,
    },
    Open {
        file: usize,
    },
    Data {
        file: usize,
        offset: u64,
    },
    Hash {
        file: usize,
        offset: u64,
        hash: String,
    },
    Checkpoint {
        file: usize,
        offset: u64,
        credit: usize,
        /// `offset` returns receive credit; only `durable` survives a restart.
        #[serde(default)]
        durable: u64,
    },
    Close {
        file: usize,
    },
    Closed {
        file: usize,
    },
    Commit,
    Saving,
    SaveProgress {
        current: u64,
        total: u64,
    },
    Complete {
        result: String,
    },
    Pause,
    Cancel,
    Stopped,
    Error {
        code: String,
        detail: String,
    },
}

pub(super) async fn json<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    anyhow::ensure!(bytes.len() <= MAX_META, "Metadata exceeds limit");
    tokio::time::timeout(IDLE, async {
        writer.write_u32_le(bytes.len() as u32).await?;
        writer.write_all(&bytes).await
    })
    .await??;
    Ok(())
}
pub(super) async fn read<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> anyhow::Result<T> {
    tokio::time::timeout(IDLE, async {
        let length = reader.read_u32_le().await? as usize;
        anyhow::ensure!(length > 0 && length <= MAX_META, "Invalid metadata length");
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).await?;
        Ok(serde_json::from_slice(&bytes)?)
    })
    .await?
}
#[cfg(any(test, not(any(target_os = "linux", target_os = "android"))))]
pub(super) async fn data<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
) -> anyhow::Result<()> {
    anyhow::ensure!(bytes.len() <= super::FRAME, "Oversized data frame");
    tokio::time::timeout(IDLE, async {
        writer.write_u32_le(bytes.len() as u32).await?;
        writer.write_all(bytes).await
    })
    .await??;
    Ok(())
}
pub(super) async fn payload<R: AsyncRead + Unpin>(reader: &mut R) -> anyhow::Result<Vec<u8>> {
    tokio::time::timeout(IDLE, async {
        let length = reader.read_u32_le().await? as usize;
        anyhow::ensure!(length > 0 && length <= super::FRAME, "Invalid data length");
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).await?;
        Ok(bytes)
    })
    .await?
}
