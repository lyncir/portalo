use portalo_core::android_storage;
use std::{io, os::fd::FromRawFd};
use tokio::{fs::File, net::TcpStream};

/// Java retains the URI and its original descriptor; Rust owns a duplicate.
/// Failed tasks explicitly abort on the blocking pool; the guard also covers
/// task cancellation between opening the document and committing its name.
struct Document(Option<i64>);

impl Document {
    fn open(tree: &str, name: &str) -> io::Result<(Self, std::fs::File)> {
        let document = Self(Some(android_storage::begin(tree, name)?));
        let descriptor = match android_storage::duplicate_fd(document.0.unwrap()) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                return match document.abort() {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(io::Error::other(format!(
                        "{error}；临时文件清理失败：{cleanup}"
                    ))),
                };
            }
        };
        if descriptor < 0 {
            return Err(io::Error::other("存储提供方返回了无效文件描述符"));
        }
        // The JNI API transfers ownership of a freshly duplicated descriptor.
        let file = unsafe { std::fs::File::from_raw_fd(descriptor) };
        Ok((document, file))
    }

    fn finish(mut self) -> io::Result<()> {
        let token = self.0.take().unwrap();
        match android_storage::finish(token) {
            Ok(()) => Ok(()),
            Err(error) => match android_storage::abort(token) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(io::Error::other(format!(
                    "{error}；临时文件清理失败：{cleanup}"
                ))),
            },
        }
    }

    fn abort(mut self) -> io::Result<()> {
        android_storage::abort(self.0.take().unwrap())
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        if let Some(token) = self.0.take() {
            // Drop can occur on a Tokio worker during cancellation. Do not run
            // a potentially blocking content provider operation on that worker.
            let _ = std::thread::Builder::new()
                .name("saf-cleanup".into())
                .spawn(move || {
                    if let Err(error) = android_storage::abort(token) {
                        bevy::log::error!("SAF cleanup: {error}");
                    }
                });
        }
    }
}

pub(super) async fn receive(
    stream: &mut TcpStream,
    tree: String,
    name: String,
    size: u64,
    progress: &mut super::ReceiveProgress,
) -> anyhow::Result<()> {
    let (document, file) =
        tokio::task::spawn_blocking(move || Document::open(&tree, &name)).await??;
    let mut file = File::from_std(file);
    let result = super::copy_exact(stream, &mut file, size, progress).await;
    drop(file);
    if let Err(error) = result {
        if let Err(cleanup) = tokio::task::spawn_blocking(move || document.abort()).await? {
            return Err(error.context(format!("未完成文件清理失败：{cleanup}")));
        }
        return Err(error);
    }
    tokio::task::spawn_blocking(move || document.finish()).await??;
    Ok(())
}
