//! Windows file-cache socket transmission and optional filesystem block cloning.
//! Only native submissions which did not start can fall back on the same frame.
//! Submitted I/O owns its handles and OVERLAPPED until completion, even if the
//! awaiting Rust future is dropped. No TF_WRITE_BEHIND or socket reuse flags.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    mem::{size_of, size_of_val},
    os::windows::io::{
        AsRawHandle, AsRawSocket, BorrowedSocket, FromRawHandle, OwnedHandle, OwnedSocket,
    },
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_ACCESS_DENIED, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
        ERROR_NOT_SAME_DEVICE, ERROR_NOT_SUPPORTED, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Networking::WinSock::{
        LPFN_TRANSMITFILE, SIO_GET_EXTENSION_FUNCTION_POINTER, WSA_IO_INCOMPLETE, WSA_IO_PENDING,
        WSAEINVAL, WSAEOPNOTSUPP, WSAGetLastError, WSAGetOverlappedResult, WSAID_TRANSMITFILE,
        WSAIoctl,
    },
    Storage::FileSystem::{
        GetDiskFreeSpaceW, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
        GetVolumePathNameW,
    },
    System::{
        IO::{CancelIoEx, DeviceIoControl, OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0},
        Ioctl::{DUPLICATE_EXTENTS_DATA, FSCTL_DUPLICATE_EXTENTS_TO_FILE},
        SystemServices::FILE_SUPPORTS_BLOCK_REFCOUNTING,
        Threading::{CreateEventW, WaitForSingleObject},
    },
};

struct Socket {
    handle: OwnedSocket,
    transmit: LPFN_TRANSMITFILE,
    bytes: AtomicU64,
}
impl Drop for Socket {
    fn drop(&mut self) {
        bevy::log::info!(
            completed_payload_bytes = self.bytes.load(Ordering::Relaxed),
            "Windows native file transmission finished"
        );
    }
}
pub(crate) struct Transmitter(Arc<Socket>);

fn unavailable(code: i32) -> bool {
    matches!(code, WSAEOPNOTSUPP | WSAEINVAL)
        || matches!(
            code as u32,
            ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_INVALID_PARAMETER
        )
}

impl Transmitter {
    pub fn new(socket: &tokio::net::TcpStream) -> io::Result<Option<Self>> {
        if std::env::var_os("PORTALO_DISABLE_SENDFILE").is_some() {
            return Ok(None);
        }
        // SAFETY: the borrowed handle lives through cloning. The duplicate owns
        // a reference to the socket independent of the Tokio halves' lifetime.
        let handle =
            unsafe { BorrowedSocket::borrow_raw(socket.as_raw_socket()) }.try_clone_to_owned()?;
        let mut transmit: LPFN_TRANSMITFILE = None;
        let mut returned = 0;
        // SAFETY: correctly sized live GUID and function-pointer storage. This
        // synchronous provider lookup initiates no file transmission.
        let result = unsafe {
            WSAIoctl(
                handle.as_raw_socket() as _,
                SIO_GET_EXTENSION_FUNCTION_POINTER,
                std::ptr::from_ref(&WSAID_TRANSMITFILE).cast(),
                size_of_val(&WSAID_TRANSMITFILE) as u32,
                (&mut transmit as *mut LPFN_TRANSMITFILE).cast(),
                size_of::<LPFN_TRANSMITFILE>() as u32,
                &mut returned,
                null_mut(),
                None,
            )
        };
        if result != 0 {
            // SAFETY: reading this thread's Winsock error immediately after failure.
            let code = unsafe { WSAGetLastError() };
            if unavailable(code) {
                return Ok(None);
            }
            return Err(io::Error::from_raw_os_error(code));
        }
        if returned as usize != size_of::<LPFN_TRANSMITFILE>() || transmit.is_none() {
            return Ok(None);
        }
        Ok(Some(Self(Arc::new(Socket {
            handle,
            transmit,
            bytes: AtomicU64::new(0),
        }))))
    }

    /// true: exactly length bytes sent; false: unsupported, no payload sent.
    /// Any error after submission terminates the connection, never retries it.
    pub async fn send(
        &self,
        file: Arc<File>,
        offset: u64,
        length: usize,
        timeout: Duration,
    ) -> io::Result<bool> {
        if length == 0 || length > 256 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid native frame length",
            ));
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        struct Cancel(Arc<AtomicBool>);
        impl Drop for Cancel {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _cancel = Cancel(cancelled.clone());
        let socket = self.0.clone();
        let started = Instant::now();
        tokio::task::spawn_blocking(move || {
            transmit(
                socket,
                file,
                offset,
                length as u32,
                started,
                timeout,
                &cancelled,
            )
        })
        .await
        .map_err(io::Error::other)?
    }
}

/// All pointer-bearing state is created and dropped on a blocking worker. The
/// event's low bit suppresses IOCP notifications: Mio must never see our private
/// OVERLAPPED as one of its own operations on an associated/duplicated socket.
struct Operation {
    socket: Arc<Socket>,
    file: Arc<File>,
    event: OwnedHandle,
    overlapped: Box<OVERLAPPED>,
    pending: bool,
}
impl Drop for Operation {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        // SAFETY: all handles and the exact submitted OVERLAPPED remain owned.
        // ERROR_NOT_FOUND is a completion race, not permission to free early.
        unsafe {
            CancelIoEx(
                self.socket.handle.as_raw_socket() as HANDLE,
                &*self.overlapped,
            );
        }
        let mut bytes = 0;
        let mut flags = 0;
        loop {
            // Wait for the request, including cancellation, to become terminal.
            // A defective provider can delay this; keeping ownership is required
            // for memory safety and avoids a kernel write to freed OVERLAPPED.
            let result = unsafe {
                WSAGetOverlappedResult(
                    self.socket.handle.as_raw_socket() as _,
                    &*self.overlapped,
                    &mut bytes,
                    1,
                    &mut flags,
                )
            };
            if result != 0 || unsafe { WSAGetLastError() } != WSA_IO_INCOMPLETE {
                break;
            }
        }
    }
}

fn transmit(
    socket: Arc<Socket>,
    file: Arc<File>,
    offset: u64,
    length: u32,
    started: Instant,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> io::Result<bool> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Native send cancelled",
        ));
    }
    if started.elapsed() >= timeout {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Native send deadline",
        ));
    }
    let size = file.metadata()?.len();
    if offset
        .checked_add(length as u64)
        .is_none_or(|end| end > size)
    {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Snapshot region is unavailable",
        ));
    }
    // SAFETY: unnamed manual-reset event, no borrowed security attributes.
    let event = unsafe { CreateEventW(null(), 1, 0, null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateEventW returned a new owned, valid handle.
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    let overlapped = Box::new(OVERLAPPED {
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: offset as u32,
                OffsetHigh: (offset >> 32) as u32,
            },
        },
        hEvent: ((event.as_raw_handle() as usize) | 1) as HANDLE,
        ..Default::default()
    });
    let mut operation = Operation {
        socket,
        file,
        event,
        overlapped,
        pending: false,
    };
    // SAFETY: provider-specific function obtained for this socket; all pointers
    // are owned until terminal completion. Each frame is below the API limit.
    let result = unsafe {
        (operation.socket.transmit.unwrap())(
            operation.socket.handle.as_raw_socket() as _,
            operation.file.as_raw_handle(),
            length,
            0,
            &mut *operation.overlapped,
            null(),
            0,
        )
    };
    if result == 0 {
        let code = unsafe { WSAGetLastError() };
        if code != WSA_IO_PENDING {
            // API contract: failure other than IO_PENDING did not initiate an
            // operation and will not deliver a completion notification.
            if unavailable(code) {
                return Ok(false);
            }
            return Err(io::Error::from_raw_os_error(code));
        }
    }
    operation.pending = true;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Native send cancelled",
            ));
        }
        if started.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Native send deadline",
            ));
        }
        // SAFETY: wait on our actual event handle, without the IOCP flag bit.
        let wait = unsafe { WaitForSingleObject(operation.event.as_raw_handle(), 50) };
        if wait == WAIT_TIMEOUT {
            continue;
        }
        if wait != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut bytes = 0;
        let mut flags = 0;
        let result = unsafe {
            WSAGetOverlappedResult(
                operation.socket.handle.as_raw_socket() as _,
                &*operation.overlapped,
                &mut bytes,
                0,
                &mut flags,
            )
        };
        if result == 0 {
            let code = unsafe { WSAGetLastError() };
            if code == WSA_IO_INCOMPLETE {
                continue;
            }
            operation.pending = false;
            return Err(io::Error::from_raw_os_error(code));
        }
        operation.pending = false;
        if bytes != length {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "Short native file transmission",
            ));
        }
        operation
            .socket
            .bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        return Ok(true);
    }
}

fn clone_unavailable(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        matches!(
            code as u32,
            ERROR_INVALID_FUNCTION
                | ERROR_NOT_SUPPORTED
                | ERROR_INVALID_PARAMETER
                | ERROR_NOT_SAME_DEVICE
                | ERROR_ACCESS_DENIED
        )
    })
}

fn volume(file: &File) -> Option<(u32, u32)> {
    let mut serial = 0;
    let mut flags = 0;
    // SAFETY: valid file handle, optional output buffers null, scalar outputs live.
    let result = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            null_mut(),
            0,
            &mut serial,
            null_mut(),
            &mut flags,
            null_mut(),
            0,
        )
    };
    (result != 0).then_some((serial, flags))
}

fn cluster_size(file: &File) -> Option<u64> {
    // SAFETY: size query has no output buffer; subsequent buffers include NUL.
    let length = unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), null_mut(), 0, 0) };
    if length == 0 || length > 32768 {
        return None;
    }
    let mut path = vec![0u16; length as usize + 1];
    let read = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            0,
        )
    };
    if read == 0 || read as usize >= path.len() {
        return None;
    }
    let mut root = vec![0u16; 32769];
    if unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return None;
    }
    let mut sectors = 0;
    let mut bytes = 0;
    let mut free = 0;
    let mut total = 0;
    if unsafe {
        GetDiskFreeSpaceW(
            root.as_ptr(),
            &mut sectors,
            &mut bytes,
            &mut free,
            &mut total,
        )
    } == 0
    {
        return None;
    }
    let size = u64::from(sectors) * u64::from(bytes);
    (size.is_power_of_two() && size <= 1024 * 1024).then_some(size)
}

/// Clone the aligned prefix on a block-refcounting volume, copy at most one
/// cluster's tail. No hard links to live user files and no source modifications.
pub(crate) fn clone_file(
    input: &mut File,
    output: &File,
    size: u64,
    keep_going: impl Fn() -> bool,
) -> io::Result<bool> {
    if output.metadata()?.len() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Block clone destination must be a new empty file",
        ));
    }
    if std::env::var_os("PORTALO_DISABLE_REFLINK").is_some()
        || size == 0
        || size > i64::MAX as u64
        || input.stream_position()? != 0
        || !input.metadata()?.is_file()
        || input.metadata()?.len() != size
    {
        return Ok(false);
    }
    let (Some((source_volume, source_flags)), Some((target_volume, target_flags))) =
        (volume(input), volume(output))
    else {
        return Ok(false);
    };
    if source_volume != target_volume
        || source_flags & target_flags & FILE_SUPPORTS_BLOCK_REFCOUNTING == 0
    {
        return Ok(false);
    }
    let Some(cluster) = cluster_size(input) else {
        return Ok(false);
    };
    let aligned = size / cluster * cluster;
    if aligned == 0 {
        return Ok(false);
    }
    output.set_len(size)?;
    let mut offset = 0;
    // Below 4 GiB per ioctl; splitting also permits cancellation between calls.
    let chunk = (64 * 1024 * 1024 / cluster) * cluster;
    while offset < aligned {
        if !keep_going() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Snapshot preparation cancelled",
            ));
        }
        let count = (aligned - offset).min(chunk);
        let request = DUPLICATE_EXTENTS_DATA {
            FileHandle: input.as_raw_handle(),
            SourceFileOffset: offset as i64,
            TargetFileOffset: offset as i64,
            ByteCount: count as i64,
        };
        let mut returned = 0;
        // SAFETY: synchronous ioctl, correctly sized live request and handles;
        // both regions are cluster-aligned and within their respective EOFs.
        let result = unsafe {
            DeviceIoControl(
                output.as_raw_handle(),
                FSCTL_DUPLICATE_EXTENTS_TO_FILE,
                (&request as *const DUPLICATE_EXTENTS_DATA).cast(),
                size_of::<DUPLICATE_EXTENTS_DATA>() as u32,
                null_mut(),
                0,
                &mut returned,
                null_mut(),
            )
        };
        if result == 0 {
            let error = io::Error::last_os_error();
            if !clone_unavailable(&error) {
                return Err(error);
            }
            // A later range may fail after earlier ranges succeeded. Remove
            // all partial clone extents before starting the normal copy path.
            output.set_len(0)?;
            let mut output = output;
            output.seek(SeekFrom::Start(0))?;
            bevy::log::debug!(%error,"Windows block clone unavailable; using copy");
            return Ok(false);
        }
        offset += count;
    }
    if !keep_going() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Snapshot preparation cancelled",
        ));
    }
    let tail = (size - aligned) as usize;
    if tail != 0 {
        let mut bytes = vec![0; tail];
        input.seek(SeekFrom::Start(aligned))?;
        input.read_exact(&mut bytes)?;
        let mut output = output;
        output.seek(SeekFrom::Start(aligned))?;
        output.write_all(&bytes)?;
        input.seek(SeekFrom::Start(0))?;
    }
    bevy::log::info!(
        cloned_bytes = aligned,
        tail_bytes = tail,
        "Windows block-cloned snapshot ready"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    unsafe extern "system" fn unsupported_transmit(
        _socket: windows_sys::Win32::Networking::WinSock::SOCKET,
        _file: HANDLE,
        _length: u32,
        _send_size: u32,
        _overlapped: *mut OVERLAPPED,
        _buffers: *const windows_sys::Win32::Networking::WinSock::TRANSMIT_FILE_BUFFERS,
        _flags: u32,
    ) -> windows_sys::core::BOOL {
        // Simulate a provider rejecting the extension before initiating I/O.
        unsafe { windows_sys::Win32::Networking::WinSock::WSASetLastError(WSAEOPNOTSUPP) };
        0
    }

    #[tokio::test]
    async fn unsupported_submission_allows_payload_only_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot");
        std::fs::write(&path, b"payload").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, receiver) = tokio::join!(
            tokio::net::TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept()
        );
        let mut sender = sender.unwrap();
        let (mut receiver, _) = receiver.unwrap();
        // SAFETY: clone while the Tokio socket is alive; the clone owns its handle.
        let handle = unsafe { BorrowedSocket::borrow_raw(sender.as_raw_socket()) }
            .try_clone_to_owned()
            .unwrap();
        let native = Transmitter(Arc::new(Socket {
            handle,
            transmit: Some(unsupported_transmit),
            bytes: AtomicU64::new(0),
        }));
        sender.write_u32_le(7).await.unwrap();
        assert!(
            !native
                .send(
                    Arc::new(File::open(path).unwrap()),
                    0,
                    7,
                    Duration::from_secs(10)
                )
                .await
                .unwrap()
        );
        sender.write_all(b"payload").await.unwrap();
        let mut received = [0; 11];
        tokio::time::timeout(Duration::from_secs(10), receiver.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&received[..4], &7u32.to_le_bytes());
        assert_eq!(&received[4..], b"payload");
    }

    #[tokio::test]
    async fn transmit_file_preserves_framing_and_large_explicit_offsets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sparse-snapshot");
        let mut source = File::options()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut returned = 0;
        // SAFETY: mark only this disposable test file sparse before constructing
        // a >4 GiB logical offset, so the test does not allocate a huge payload.
        assert_ne!(
            unsafe {
                DeviceIoControl(
                    source.as_raw_handle(),
                    windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE,
                    null(),
                    0,
                    null_mut(),
                    0,
                    &mut returned,
                    null_mut(),
                )
            },
            0
        );
        let offset = (1u64 << 32) + 12347;
        let bytes = vec![73; 256 * 1024 + 17];
        source.seek(SeekFrom::Start(offset)).unwrap();
        source.write_all(&bytes).unwrap();
        source.sync_all().unwrap();
        let source = Arc::new(source);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, receiver) = tokio::join!(
            tokio::net::TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept()
        );
        let mut sender = sender.unwrap();
        let (mut receiver, _) = receiver.unwrap();
        let Some(native) = Transmitter::new(&sender).unwrap() else {
            eprintln!("TransmitFile unavailable or explicitly disabled; native-path case skipped");
            return;
        };
        let send = tokio::spawn(async move {
            sender.write_all(b"HEAD").await.unwrap();
            assert!(
                native
                    .send(source.clone(), offset, 256 * 1024, Duration::from_secs(10))
                    .await
                    .unwrap()
            );
            assert!(
                native
                    .send(source, offset + 256 * 1024, 17, Duration::from_secs(10))
                    .await
                    .unwrap()
            );
            sender.write_all(b"TAIL").await.unwrap();
        });
        let mut received = vec![0; bytes.len() + 8];
        tokio::time::timeout(Duration::from_secs(15), receiver.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();
        send.await.unwrap();
        assert_eq!(&received[..4], b"HEAD");
        assert_eq!(&received[4..received.len() - 4], bytes);
        assert_eq!(&received[received.len() - 4..], b"TAIL");
    }

    #[tokio::test]
    async fn expired_native_deadline_submits_no_payload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot");
        std::fs::write(&path, b"payload").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, receiver) = tokio::join!(
            tokio::net::TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept()
        );
        let sender = sender.unwrap();
        let (receiver, _) = receiver.unwrap();
        let Some(native) = Transmitter::new(&sender).unwrap() else {
            return;
        };
        let error = native
            .send(Arc::new(File::open(path).unwrap()), 0, 7, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            receiver.try_read(&mut [0; 1]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn clone_tail_and_source_destination_are_independent() {
        // Point this at a disposable directory on ReFS to require the native
        // path. The default test temp volume can legitimately use fallback.
        let required = std::env::var_os("PORTALO_REFS_TEST_DIR");
        let directory = match &required {
            Some(path) => tempfile::tempdir_in(path).unwrap(),
            None => tempfile::tempdir().unwrap(),
        };
        let bytes = vec![19; 2 * 1024 * 1024 + 123];
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        std::fs::write(&source, &bytes).unwrap();
        let mut input = File::open(&source).unwrap();
        let mut output = File::options()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&target)
            .unwrap();
        let cloned = clone_file(&mut input, &output, bytes.len() as u64, || true).unwrap();
        if required.is_some() {
            assert!(cloned, "Requested ReFS native test did not clone");
        }
        if !cloned {
            assert_eq!(output.metadata().unwrap().len(), 0);
            std::io::copy(&mut input, &mut output).unwrap();
        }
        output.sync_all().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        let mut original = File::options().write(true).open(&source).unwrap();
        original.write_all(b"SOURCE").unwrap();
        original.sync_all().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        output.seek(SeekFrom::Start(1024 * 1024)).unwrap();
        output.write_all(b"TARGET").unwrap();
        output.sync_all().unwrap();
        let source_bytes = std::fs::read(source).unwrap();
        assert_eq!(&source_bytes[..6], b"SOURCE");
        assert_eq!(
            &source_bytes[1024 * 1024..1024 * 1024 + 6],
            &bytes[1024 * 1024..1024 * 1024 + 6]
        );
    }
}
