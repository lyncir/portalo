use super::*;
use portalo_core::{TransferProgressMsg, TransferStatus, settings::ReceiveDestination};
use std::{fs, io::Write};
use tokio::{net::TcpListener, sync::watch};

fn manifest(bytes: &[u8]) -> Manifest {
    Manifest {
        streaming: false,
        id: uuid::Uuid::new_v4().to_string(),
        name: "test".into(),
        layout: Layout::Legacy,
        entries: vec![Entry {
            path: vec!["文件.bin".into()],
            directory: false,
            size: bytes.len() as u64,
            blocks: bytes
                .chunks(BLOCK)
                .map(|b| blake3::hash(b).to_hex().to_string())
                .collect(),
        }],
    }
}
#[test]
fn durable_checkpoint_revalidates_prefix_and_recovers_commit_gap() {
    let state = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let bytes = vec![7; BLOCK + 31];
    let manifest = manifest(&bytes);
    let token = "a".repeat(64);
    let session = storage::Session::open_at(
        state.path().to_owned(),
        manifest.clone(),
        token.clone(),
        ReceiveDestination::Directory(destination.path().to_owned()),
    )
    .unwrap();
    assert!(session.block(0, 0, &vec![8; BLOCK]).is_err());
    session.block(0, 0, &bytes[..BLOCK]).unwrap();
    session.flush().unwrap();
    let journal = session.journal.clone();
    let root = session.root.clone();
    drop(session);
    // Uncheckpointed suffix must be discarded on restart.
    let path = manifest.path(&root, 0);
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b"unconfirmed")
        .unwrap();
    let session = storage::Session::open_at(
        state.path().to_owned(),
        manifest.clone(),
        token.clone(),
        ReceiveDestination::Directory(destination.path().to_owned()),
    )
    .unwrap();
    assert_eq!(session.receipt.lock().unwrap().offsets[0], BLOCK as u64);
    assert!(
        storage::Session::open_at(
            state.path().to_owned(),
            manifest.clone(),
            token.clone(),
            ReceiveDestination::Directory(destination.path().to_owned())
        )
        .is_err()
    );
    session.block(0, BLOCK as u64, &bytes[BLOCK..]).unwrap();
    let result = session.commit().unwrap();
    assert_eq!(
        fs::read(manifest.path(std::path::Path::new(&result), 0)).unwrap(),
        bytes
    );
    let mut receipt: model::Receipt = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    receipt.result = None;
    storage::save(&journal, &receipt).unwrap();
    drop(session);
    let recovered = storage::Session::open_at(
        state.path().to_owned(),
        manifest.clone(),
        token,
        ReceiveDestination::Directory(destination.path().to_owned()),
    )
    .unwrap();
    assert_eq!(recovered.commit().unwrap(), result);
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 1);
}

#[test]
fn manifest_rejects_conflicts_traversal_and_special_names() {
    for name in ["..", "C:foo", "NUL", "a/b", ".portalo-receipt"] {
        let mut m = manifest(b"abc");
        m.entries[0].path = vec![name.into()];
        assert!(m.validate().is_err());
    }
    let mut m = manifest(b"abc");
    let mut duplicate = m.entries[0].clone();
    duplicate.path = vec!["文件.BIN".into()];
    m.entries.push(duplicate);
    assert!(m.validate().is_err());
}

#[test]
fn private_export_snapshot_survives_picker_cleanup_without_copy() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("export");
    fs::create_dir_all(cache.join("nested/empty")).unwrap();
    fs::write(cache.join("nested/data"), b"private export").unwrap();
    let probe = cache.join("link-probe");
    let supports_links = fs::hard_link(cache.join("nested/data"), &probe).is_ok();
    if supports_links {
        fs::remove_file(probe).unwrap();
    }
    let source = crate::SendSource::cached(cache.clone(), "folder".into()).unwrap();
    let owner = source.clone();
    let (_commands, control) = watch::channel(Command::Run);
    let task = prepare(vec![source], vec![], &control).unwrap();
    let id = task
        .manifest
        .entries
        .iter()
        .position(|entry| !entry.directory)
        .unwrap();
    let snapshot = storage::outgoing(&task.manifest.id)
        .join("data")
        .join(id.to_string());
    // Both temporary directories are on the same default test filesystem.
    if supports_links {
        assert!(same_file::is_same_file(cache.join("nested/data"), &snapshot).unwrap());
    }
    drop(owner);
    assert!(!cache.exists());
    assert_eq!(fs::read(snapshot).unwrap(), b"private export");
    assert!(
        task.manifest
            .entries
            .iter()
            .any(|entry| entry.directory && entry.path == ["nested", "empty"])
    );
    discard(&task.manifest.id).unwrap();
}

#[test]
fn parallel_writers_and_checkpoint_generations_recover_complete_bytes() {
    let state = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let bytes = vec![23; 3 * BLOCK + 7];
    let mut m = manifest(&bytes);
    let mut second = m.entries[0].clone();
    second.path = vec!["second".into()];
    m.entries.push(second);
    let open = || {
        storage::Session::open_at(
            state.path().to_owned(),
            m.clone(),
            "a".repeat(64),
            ReceiveDestination::Directory(destination.path().to_owned()),
        )
        .unwrap()
    };
    let session = open();
    std::thread::scope(|scope| {
        for id in 0..2 {
            let session = &session;
            let bytes = &bytes;
            scope.spawn(move || {
                let payload = session.open_payload(id).unwrap();
                for (index, block) in bytes.chunks(BLOCK).enumerate() {
                    session
                        .write_block(&payload, (index * BLOCK) as u64, block)
                        .unwrap();
                    session.flush().unwrap();
                }
            });
        }
    });
    session.flush().unwrap();
    drop(session);
    let session = open();
    assert_eq!(
        session.receipt.lock().unwrap().offsets,
        vec![bytes.len() as u64; 2]
    );
    let result = session.commit().unwrap();
    for id in 0..2 {
        assert_eq!(
            fs::read(m.path(std::path::Path::new(&result), id)).unwrap(),
            bytes
        );
    }
}

#[test]
fn batched_checkpoint_and_unpublished_tail_recovery() {
    let state = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let bytes = vec![19; 9 * BLOCK];
    let m = manifest(&bytes);
    let open = || {
        storage::Session::open_at(
            state.path().to_owned(),
            m.clone(),
            "a".repeat(64),
            ReceiveDestination::Directory(destination.path().to_owned()),
        )
        .unwrap()
    };
    let s = open();
    for id in 0..8 {
        s.block(0, (id * BLOCK) as u64, &bytes[id * BLOCK..(id + 1) * BLOCK])
            .unwrap();
    }
    // The byte threshold must commit even without a transport timer. Earlier
    // timed commits are possible on slow disks; flush fixes the baseline.
    assert!(s.receipt.lock().unwrap().offsets[0] >= BLOCK as u64);
    s.flush().unwrap();
    assert_eq!(s.receipt.lock().unwrap().offsets[0], (8 * BLOCK) as u64);
    let journal: model::Receipt = serde_json::from_slice(&fs::read(&s.journal).unwrap()).unwrap();
    assert_eq!(journal.offsets[0], (8 * BLOCK) as u64);
    // Emulate data reaching storage before its journal replacement.
    fs::OpenOptions::new()
        .append(true)
        .open(m.path(&s.root, 0))
        .unwrap()
        .write_all(&bytes[8 * BLOCK..])
        .unwrap();
    drop(s);
    let s = open();
    assert_eq!(
        fs::metadata(m.path(&s.root, 0)).unwrap().len(),
        (8 * BLOCK) as u64
    );
    assert!(s.commit().is_err());
    s.block(0, (8 * BLOCK) as u64, &bytes[8 * BLOCK..]).unwrap();
    let result = s.commit().unwrap();
    assert_eq!(
        fs::read(m.path(std::path::Path::new(&result), 0)).unwrap(),
        bytes
    );
}

#[test]
fn markerless_publication_rejects_changed_tree_and_preserves_equal_collision() {
    let state = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let mut m = manifest(b"original");
    m.layout = Layout::Directory;
    let existing = destination.path().join(&m.name);
    fs::create_dir(&existing).unwrap();
    fs::write(m.path(&existing, 0), b"original").unwrap();
    let open = || {
        storage::Session::open_at(
            state.path().to_owned(),
            m.clone(),
            "a".repeat(64),
            ReceiveDestination::Directory(destination.path().to_owned()),
        )
    };
    let s = open().unwrap();
    assert!(s.receipt.lock().unwrap().result.is_none());
    s.block(0, 0, b"original").unwrap();
    let result = std::path::PathBuf::from(s.commit().unwrap());
    assert_ne!(result, existing);
    assert!(!result.join(".portalo-receipt").exists());
    let mut receipt = s.receipt.lock().unwrap().clone();
    receipt.result = None;
    storage::save(&s.journal, &receipt).unwrap();
    drop(s);
    for name in ["unexpected", ".portalo-receipt"] {
        fs::write(result.join(name), b"extra").unwrap();
        assert!(open().is_err());
        fs::remove_file(result.join(name)).unwrap();
    }
    fs::write(m.path(&result, 0), b"modified").unwrap();
    assert!(open().is_err());
    fs::write(m.path(&result, 0), b"original").unwrap();
    assert_eq!(
        std::path::PathBuf::from(open().unwrap().commit().unwrap()),
        result
    );
    assert_eq!(fs::read(m.path(&existing, 0)).unwrap(), b"original");
}

#[test]
fn single_file_collision_and_crash_after_link_recover_without_duplicate() {
    let state = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let mut m = manifest(b"new bytes");
    m.name = "photo.png".into();
    m.entries[0].path = vec![m.name.clone()];
    m.layout = Layout::File;
    fs::write(destination.path().join("photo.png"), b"existing").unwrap();
    let open = || {
        storage::Session::open_at(
            state.path().to_owned(),
            m.clone(),
            "a".repeat(64),
            ReceiveDestination::Directory(destination.path().to_owned()),
        )
        .unwrap()
    };
    let s = open();
    s.block(0, 0, b"new bytes").unwrap();
    s.flush().unwrap();
    // Simulate the actual crash window: intent persisted, hard link published,
    // completion record not yet written and staging still exists.
    {
        let mut receipt = s.receipt.lock().unwrap();
        receipt.target = destination.path().join("photo.png.1");
        receipt.publishing = true;
        storage::save(&s.journal, &*receipt).unwrap();
        fs::hard_link(m.path(&s.root, 0), &receipt.target).unwrap();
    }
    drop(s);
    let s = open();
    let result = s.commit().unwrap();
    assert_eq!(
        std::path::Path::new(&result),
        destination.path().join("photo.png.1")
    );
    assert_eq!(fs::read(&result).unwrap(), b"new bytes");
    assert_eq!(
        fs::read(destination.path().join("photo.png")).unwrap(),
        b"existing"
    );
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 2);
    let mut second = m.clone();
    second.id = uuid::Uuid::new_v4().to_string();
    let s = storage::Session::open_at(
        state.path().to_owned(),
        second,
        "b".repeat(64),
        ReceiveDestination::Directory(destination.path().to_owned()),
    )
    .unwrap();
    s.block(0, 0, b"new bytes").unwrap();
    assert_eq!(
        std::path::Path::new(&s.commit().unwrap()),
        destination.path().join("photo.png.2")
    );
    assert!(!s.root.exists());
}

#[test]
fn single_file_rename_publication_recovers_and_rejects_changed_content() {
    for streaming in [false, true] {
        for corrupt in [false, true] {
            let state = tempfile::tempdir().unwrap();
            let destination = tempfile::tempdir().unwrap();
            let data = b"new bytes";
            let mut m = manifest(data);
            m.name = "photo.png".into();
            m.entries[0].path = vec![m.name.clone()];
            m.layout = Layout::File;
            m.streaming = streaming;
            if streaming {
                m.entries[0].blocks.clear();
            }
            let open = || {
                storage::Session::open_at(
                    state.path().to_owned(),
                    m.clone(),
                    "a".repeat(64),
                    ReceiveDestination::Directory(destination.path().to_owned()),
                )
            };
            let s = open().unwrap();
            if streaming {
                let payload = s.open_payload(0).unwrap();
                s.write_stream_block(&payload, 0, data, &blake3::hash(data).to_hex().to_string())
                    .unwrap();
            } else {
                s.block(0, 0, data).unwrap();
            }
            s.flush().unwrap();
            let target = destination.path().join("photo.png.1");
            let existing = destination.path().join("photo.png");
            fs::write(&existing, b"user file").unwrap();
            let source = m.path(&s.root, 0);
            assert!(storage::publish(&source, &existing).is_err());
            assert!(source.exists());
            {
                let mut receipt = s.receipt.lock().unwrap();
                receipt.target = target.clone();
                receipt.publishing = true;
                storage::save(&s.journal, &*receipt).unwrap();
                storage::publish(&source, &target).unwrap();
            }
            assert!(!source.exists());
            drop(s);
            if corrupt {
                fs::write(&target, b"bad bytes").unwrap();
                assert!(open().is_err());
                assert_eq!(fs::read(&target).unwrap(), b"bad bytes");
            } else {
                let restored = open().unwrap();
                assert_eq!(std::path::Path::new(&restored.commit().unwrap()), target);
                assert_eq!(fs::read(&target).unwrap(), data);
                assert!(!restored.root.exists());
            }
            assert_eq!(fs::read(&existing).unwrap(), b"user file");
            // Failed verification retains the task's staging directory so the
            // user can inspect/recover it; successful publication cleans it.
            assert_eq!(
                fs::read_dir(destination.path()).unwrap().count(),
                if corrupt { 3 } else { 2 }
            );
        }
    }
}

#[test]
fn legacy_manifest_digest_is_unchanged() {
    let m = manifest(b"abc");
    let json = serde_json::to_vec(&m).unwrap();
    assert!(!String::from_utf8_lossy(&json).contains("layout"));
    let restored: Manifest = serde_json::from_slice(&json).unwrap();
    assert_eq!(restored.layout, Layout::Legacy);
    assert_eq!(m.digest().unwrap(), restored.digest().unwrap());
}

#[test]
fn directory_original_name_collision_and_publication_gap() {
    let state = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    fs::create_dir(destination.path().join("photos")).unwrap();
    let m = Manifest {
        streaming: false,
        id: uuid::Uuid::new_v4().to_string(),
        name: "photos".into(),
        entries: vec![],
        layout: Layout::Directory,
    };
    let open = || {
        storage::Session::open_at(
            state.path().to_owned(),
            m.clone(),
            "d".repeat(64),
            ReceiveDestination::Directory(destination.path().to_owned()),
        )
        .unwrap()
    };
    let s = open();
    let result = s.commit().unwrap();
    assert_eq!(
        std::path::Path::new(&result),
        destination.path().join("photos.1")
    );
    let journal = s.journal.clone();
    drop(s);
    let mut receipt: model::Receipt = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    receipt.result = None;
    storage::save(&journal, &receipt).unwrap();
    assert_eq!(open().commit().unwrap(), result);
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 2);
}

async fn listen(
    task: &mut SavedTask,
    destination: std::path::PathBuf,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    task.addresses = vec![listener.local_addr().unwrap()];
    tokio::spawn(async move {
        crate::start_file_receiver(listener.accept().await?.0, destination).await
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streamed_preparation_transfers_parallel_files_and_empty_directories() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let folder = source.path().join("streamed");
    fs::create_dir_all(folder.join("nested/empty")).unwrap();
    for id in 0..4 {
        fs::write(
            folder.join(format!("{id}.bin")),
            vec![id as u8; 2 * BLOCK + 17],
        )
        .unwrap();
    }
    fs::write(folder.join("zero"), []).unwrap();
    let (_tx, control) = watch::channel(Command::Run);
    let mut task = prepare_streaming_with_progress(
        vec![crate::SendSource::Path(folder)],
        vec![],
        &control,
        &|_, _| {},
    )
    .unwrap();
    assert!(
        task.manifest.streaming
            && task
                .manifest
                .entries
                .iter()
                .all(|entry| entry.blocks.is_empty())
    );
    let receiver = listen(&mut task, destination.path().to_owned()).await;
    let (progress, _) = watch::channel(TransferProgressMsg::default());
    send(task.clone(), progress, 1, control).await.unwrap();
    receiver.await.unwrap().unwrap();
    for id in 0..4 {
        assert_eq!(
            fs::read(destination.path().join(format!("streamed/{id}.bin"))).unwrap(),
            vec![id as u8; 2 * BLOCK + 17]
        );
    }
    assert!(destination.path().join("streamed/nested/empty").is_dir());
    assert_eq!(
        fs::metadata(destination.path().join("streamed/zero"))
            .unwrap()
            .len(),
        0
    );
    assert!(!storage::outgoing(&task.manifest.id).join("data").exists());
    discard(&task.manifest.id).unwrap();
}

#[test]
fn streamed_hash_checkpoint_rewinds_corruption_and_recovers_publication() {
    let source = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let bytes = vec![17; 2 * BLOCK + 31];
    fs::write(source.path().join("payload"), &bytes).unwrap();
    let (_tx, control) = watch::channel(Command::Run);
    let task = prepare_streaming_with_progress(
        vec![crate::SendSource::Path(source.path().to_owned())],
        vec![],
        &control,
        &|_, _| {},
    )
    .unwrap();
    let open = || {
        storage::Session::open_at(
            state.path().to_owned(),
            task.manifest.clone(),
            task.token.clone(),
            ReceiveDestination::Directory(destination.path().to_owned()),
        )
    };
    let snapshot = storage::outgoing(&task.manifest.id).join("data/0");
    let session = open().unwrap();
    let payload = session.open_payload(0).unwrap();
    let (chunk, hash) = super::preparation::block(&snapshot, bytes.len() as u64, 0).unwrap();
    assert!(
        session
            .write_stream_block(&payload, 0, &chunk, &"0".repeat(64))
            .is_err()
    );
    session
        .write_stream_block(&payload, 0, &chunk, &hash)
        .unwrap();
    session.flush().unwrap();
    let path = task.manifest.path(&session.root, 0);
    drop(payload);
    drop(session);
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .write_all(b"!")
        .unwrap();
    let session = open().unwrap();
    assert_eq!(session.receipt.lock().unwrap().offsets[0], 0);
    assert!(session.receipt.lock().unwrap().hashes[0].is_empty());
    let payload = session.open_payload(0).unwrap();
    for offset in (0..bytes.len()).step_by(BLOCK) {
        let (chunk, hash) =
            super::preparation::block(&snapshot, bytes.len() as u64, offset as u64).unwrap();
        session
            .write_stream_block(&payload, offset as u64, &chunk, &hash)
            .unwrap();
    }
    drop(payload);
    let result = session.commit().unwrap();
    assert_eq!(
        fs::read(task.manifest.path(std::path::Path::new(&result), 0)).unwrap(),
        bytes
    );
    let mut record = session.receipt.lock().unwrap().clone();
    record.result = None;
    storage::save(&session.journal, &record).unwrap();
    drop(session);
    assert_eq!(open().unwrap().commit().unwrap(), result);
    discard(&task.manifest.id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn received_directory_can_be_forwarded_without_internal_markers() {
    let source = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let folder = source.path().join("UCDownloads");
    fs::create_dir_all(folder.join("nested/empty")).unwrap();
    fs::write(folder.join("android.apk"), b"apk payload").unwrap();
    fs::write(folder.join("nested/.user-hidden"), b"keep user files").unwrap();
    let (_control, rx) = watch::channel(Command::Run);
    let mut task = prepare(vec![crate::SendSource::Path(folder.clone())], vec![], &rx).unwrap();
    let receiver = listen(&mut task, first.path().to_owned()).await;
    let (progress, _) = watch::channel(TransferProgressMsg::default());
    send(task.clone(), progress.clone(), 1, rx.clone())
        .await
        .unwrap();
    receiver.await.unwrap().unwrap();
    let received = first.path().join("UCDownloads");
    assert!(!received.join(".portalo-receipt").exists());
    let mut forwarded =
        prepare(vec![crate::SendSource::Path(received.clone())], vec![], &rx).unwrap();
    assert!(
        forwarded
            .manifest
            .entries
            .iter()
            .all(|entry| !entry.path.iter().any(|p| p == ".portalo-receipt"))
    );
    let receiver = listen(&mut forwarded, second.path().to_owned()).await;
    send(forwarded.clone(), progress, 2, rx).await.unwrap();
    receiver.await.unwrap().unwrap();
    assert_eq!(
        fs::read(second.path().join("UCDownloads/android.apk")).unwrap(),
        b"apk payload"
    );
    assert_eq!(
        fs::read(second.path().join("UCDownloads/nested/.user-hidden")).unwrap(),
        b"keep user files"
    );
    assert!(second.path().join("UCDownloads/nested/empty").is_dir());
    assert!(!received.join(".portalo-receipt").exists());
    discard(&task.manifest.id).unwrap();
    discard(&forwarded.manifest.id).unwrap();
}

#[test]
fn reserved_user_paths_fail_before_copying_and_include_the_path() {
    let source = tempfile::tempdir().unwrap();
    let file = source.path().join(".portalo-user-data");
    fs::write(&file, b"must not silently skip").unwrap();
    let (_control, rx) = watch::channel(Command::Run);
    let error = super::storage::prepare_with_progress(
        vec![crate::SendSource::Path(source.path().to_owned())],
        vec![],
        &rx,
        &|_, _| panic!("Invalid path must not be copied"),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains(file.to_str().unwrap()));
    fs::remove_file(file).unwrap();
    let marker = source.path().join(".portalo-receipt");
    fs::write(&marker, b"reserved entry must not be silently skipped").unwrap();
    assert!(
        prepare(
            vec![crate::SendSource::Path(source.path().to_owned())],
            vec![],
            &rx
        )
        .is_err()
    );
    assert!(marker.is_file());
    fs::remove_file(marker).unwrap();
    fs::create_dir(source.path().join(".portalo-receipt")).unwrap();
    assert!(
        prepare(
            vec![crate::SendSource::Path(source.path().to_owned())],
            vec![],
            &rx
        )
        .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn folder_parallel_pause_restart_and_completion_deduplication() {
    folder_pause_resume(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streaming_folder_pause_restart_and_completion_deduplication() {
    folder_pause_resume(true).await;
}

async fn folder_pause_resume(streaming: bool) {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("folder/empty/sub")).unwrap();
    for i in 0..6 {
        fs::write(
            source.path().join(format!("folder/{i}.bin")),
            vec![i as u8; BLOCK * 3 + 17],
        )
        .unwrap();
    }
    fs::write(source.path().join("folder/zero"), []).unwrap();
    let (control, rx) = watch::channel(Command::Run);
    let path = source.path().join("folder");
    let mut task = if streaming {
        prepare_streaming_with_progress(
            vec![crate::SendSource::Path(path)],
            vec![],
            &rx,
            &|_, _| {},
        )
    } else {
        prepare(vec![crate::SendSource::Path(path)], vec![], &rx)
    }
    .unwrap();
    let receiver = listen(&mut task, destination.path().to_owned()).await;
    let (progress, mut updates) = watch::channel(TransferProgressMsg::default());
    let pause = control.clone();
    let watcher = tokio::spawn(async move {
        loop {
            updates.changed().await.unwrap();
            if updates.borrow().current >= BLOCK as u64
                && updates.borrow().status == TransferStatus::Sending
            {
                pause.send_replace(Command::Pause);
                break;
            }
        }
    });
    send(task.clone(), progress.clone(), 1, rx.clone())
        .await
        .unwrap();
    watcher.await.unwrap();
    assert!(receiver.await.unwrap().is_err());
    control.send_replace(Command::Run);
    let receiver = listen(&mut task, destination.path().to_owned()).await;
    send(task.clone(), progress.clone(), 2, rx.clone())
        .await
        .unwrap();
    receiver.await.unwrap().unwrap();
    assert_eq!(progress.borrow().status, TransferStatus::Complete);
    let directories: Vec<_> = fs::read_dir(destination.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(directories.len(), 1);
    assert!(directories[0].join("empty/sub").is_dir());
    for i in 0..6 {
        assert_eq!(
            fs::read(directories[0].join(format!("{i}.bin"))).unwrap(),
            vec![i as u8; BLOCK * 3 + 17]
        );
    }
    let receiver = listen(&mut task, destination.path().to_owned()).await;
    send(task.clone(), progress, 3, rx).await.unwrap();
    receiver.await.unwrap().unwrap();
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 1);
    discard(&task.manifest.id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_task_removes_payload_and_empty_folder_commits() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let (control, rx) = watch::channel(Command::Run);
    let mut task = prepare(
        vec![crate::SendSource::Path(source.path().to_owned())],
        vec![],
        &rx,
    )
    .unwrap();
    let receiver = listen(&mut task, destination.path().to_owned()).await;
    let (progress, _) = watch::channel(TransferProgressMsg::default());
    send(task.clone(), progress.clone(), 1, rx.clone())
        .await
        .unwrap();
    receiver.await.unwrap().unwrap();
    assert_eq!(progress.borrow().status, TransferStatus::Complete);
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 1);
    discard(&task.manifest.id).unwrap();

    fs::write(source.path().join("payload"), vec![1; BLOCK]).unwrap();
    let mut task = prepare(
        vec![crate::SendSource::Path(source.path().to_owned())],
        vec![],
        &rx,
    )
    .unwrap();
    let receiver = listen(&mut task, destination.path().to_owned()).await;
    control.send_replace(Command::Cancel);
    send(task.clone(), progress.clone(), 2, rx).await.unwrap();
    assert!(receiver.await.unwrap().is_err());
    assert_eq!(progress.borrow().status, TransferStatus::Cancelled);
    assert!(!storage::outgoing(&task.manifest.id).exists());
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 1);
}

#[test]
fn snapshot_is_stable_and_corrupt_receiver_checkpoint_is_rewound() {
    let source = tempfile::tempdir().unwrap();
    let path = source.path().join("input");
    fs::write(&path, b"original").unwrap();
    let (_, rx) = watch::channel(Command::Run);
    let task = prepare(vec![crate::SendSource::Path(path.clone())], vec![], &rx).unwrap();
    fs::write(path, b"modified").unwrap();
    assert_eq!(
        fs::read(storage::outgoing(&task.manifest.id).join("data/0")).unwrap(),
        b"original"
    );
    discard(&task.manifest.id).unwrap();

    let state = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let manifest = manifest(b"original");
    let token = "b".repeat(64);
    let open = |token: String| {
        storage::Session::open_at(
            state.path().to_owned(),
            manifest.clone(),
            token,
            ReceiveDestination::Directory(destination.path().to_owned()),
        )
    };
    let session = open(token.clone()).unwrap();
    session.block(0, 0, b"original").unwrap();
    session.flush().unwrap();
    fs::write(manifest.path(&session.root, 0), b"modified").unwrap();
    drop(session);
    assert!(open("c".repeat(64)).is_err());
    let session = open(token).unwrap();
    assert_eq!(session.receipt.lock().unwrap().offsets[0], 0);
    assert_eq!(
        fs::metadata(manifest.path(&session.root, 0)).unwrap().len(),
        0
    );
    assert!(session.commit().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broken_connection_resumes_only_durable_blocks() {
    use tokio::io::AsyncWriteExt;
    use wire::Message;
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let bytes = vec![9; BLOCK + FRAME];
    let path = source.path().join("input");
    fs::write(&path, &bytes).unwrap();
    let (_, rx) = watch::channel(Command::Run);
    let mut task = prepare(vec![crate::SendSource::Path(path)], vec![], &rx).unwrap();
    let receiver = listen(&mut task, destination.path().to_owned()).await;
    let mut socket = tokio::net::TcpStream::connect(task.addresses[0])
        .await
        .unwrap();
    socket.write_all(MAGIC).await.unwrap();
    wire::json(&mut socket, &Message::Hello { version: 2 })
        .await
        .unwrap();
    assert!(matches!(
        wire::read(&mut socket).await.unwrap(),
        Message::Ready { .. }
    ));
    wire::json(
        &mut socket,
        &Message::Begin {
            manifest: task.manifest.clone(),
            token: task.token.clone(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        wire::read(&mut socket).await.unwrap(),
        Message::Resume { .. }
    ));
    wire::json(&mut socket, &Message::Open { file: 0 })
        .await
        .unwrap();
    for (index, chunk) in bytes[..BLOCK].chunks(FRAME).enumerate() {
        wire::json(
            &mut socket,
            &Message::Data {
                file: 0,
                offset: (index * FRAME) as u64,
            },
        )
        .await
        .unwrap();
        wire::data(&mut socket, chunk).await.unwrap();
    }
    assert!(
        matches!(wire::read(&mut socket).await.unwrap(), Message::Checkpoint { offset, .. } if offset == BLOCK as u64)
    );
    // Disconnect halfway through the next wire frame, before its checkpoint.
    wire::json(
        &mut socket,
        &Message::Data {
            file: 0,
            offset: BLOCK as u64,
        },
    )
    .await
    .unwrap();
    socket.write_u32_le(FRAME as u32).await.unwrap();
    socket.write_all(&bytes[..17]).await.unwrap();
    drop(socket);
    assert!(receiver.await.unwrap().is_err());
    let receiver = listen(&mut task, destination.path().to_owned()).await;
    let (progress, _) = watch::channel(TransferProgressMsg::default());
    send(task.clone(), progress, 1, rx).await.unwrap();
    receiver.await.unwrap().unwrap();
    let root = fs::read_dir(destination.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(fs::read(root).unwrap(), bytes);
    discard(&task.manifest.id).unwrap();
}
