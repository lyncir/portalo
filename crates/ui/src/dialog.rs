//! Native desktop dialogs run on Bevy's I/O pool without blocking rendering.
use bevy::{
    prelude::*,
    tasks::{IoTaskPool, Task, block_on, poll_once},
};
use std::path::PathBuf;

#[derive(Resource, Default)]
pub(super) struct Dialogs {
    file: Option<Task<Option<PathBuf>>>,
    directory: Option<Task<Option<PathBuf>>>,
}

#[derive(Message)]
pub(super) struct DirectoryPicked(pub Option<PathBuf>);

impl Dialogs {
    pub fn file(&mut self) {
        if self.file.is_none() {
            self.file = Some(IoTaskPool::get().spawn(async {
                rfd::AsyncFileDialog::new()
                    .pick_file()
                    .await
                    .map(|file| file.path().to_owned())
            }));
        }
    }

    pub fn directory(&mut self) {
        if self.directory.is_none() {
            self.directory = Some(IoTaskPool::get().spawn(async {
                rfd::AsyncFileDialog::new()
                    .pick_folder()
                    .await
                    .map(|file| file.path().to_owned())
            }));
        }
    }
}

pub(super) fn poll(
    mut dialogs: ResMut<Dialogs>,
    mut files: MessageWriter<super::PickedFile>,
    mut directories: MessageWriter<DirectoryPicked>,
) {
    if let Some(task) = dialogs.file.as_mut()
        && let Some(path) = block_on(poll_once(task))
    {
        dialogs.file = None;
        files.write(super::PickedFile {
            source: path.map(portalo_network::SendSource::Path),
        });
    }
    if let Some(task) = dialogs.directory.as_mut()
        && let Some(path) = block_on(poll_once(task))
    {
        dialogs.directory = None;
        directories.write(DirectoryPicked(path));
    }
}
