//! Android SAF bridge. The Java side streams content URIs into private cache
//! on a worker, never loading a complete file into memory.
use bevy::prelude::*;
use jni::{
    JNIEnv, JavaVM,
    objects::{JObject, JString},
};
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
static BACK: AtomicBool = AtomicBool::new(false);
static SAFE_INSETS: AtomicU64 = AtomicU64::new(0);

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_bevyengine_portalo_MainActivity_nativeInsetsChanged(
    _env: JNIEnv,
    _activity: JObject,
    top: f32,
    bottom: f32,
) {
    SAFE_INSETS.store(
        ((top.to_bits() as u64) << 32) | bottom.to_bits() as u64,
        Ordering::Relaxed,
    );
}

pub(super) fn update_insets(
    mut columns: Query<&mut Node, With<super::widgets::PageColumn>>,
    added: Query<Entity, Added<super::widgets::PageColumn>>,
    mut previous: Local<u64>,
) {
    let insets = SAFE_INSETS.load(Ordering::Relaxed);
    if insets == *previous && added.is_empty() {
        return;
    }
    *previous = insets;
    let top = f32::from_bits((insets >> 32) as u32).max(0.0);
    let bottom = f32::from_bits(insets as u32).max(0.0);
    for mut node in &mut columns {
        node.padding = UiRect {
            left: px(20),
            right: px(20),
            top: px(20.0 + top),
            bottom: px(20.0 + bottom),
        };
    }
}

static PICKED: Mutex<Option<Option<PathBuf>>> = Mutex::new(None);
pub(super) type DirectoryResult = Result<Option<portalo_core::settings::DocumentTree>, String>;
static DIRECTORY: Mutex<Option<DirectoryResult>> = Mutex::new(None);

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_bevyengine_portalo_MainActivity_nativeDirectoryPicked(
    mut env: JNIEnv,
    _activity: JObject,
    uri: JString,
    label: JString,
    error: JString,
) {
    let result = (|| -> jni::errors::Result<DirectoryResult> {
        let uri: String = env.get_string(&uri)?.into();
        let label: String = env.get_string(&label)?.into();
        let error: String = env.get_string(&error)?.into();
        Ok(if !error.is_empty() {
            Err(error)
        } else if uri.is_empty() {
            Ok(None)
        } else {
            Ok(Some(portalo_core::settings::DocumentTree { uri, label }))
        })
    })()
    .unwrap_or_else(|error| Err(format!("无法读取目录选择结果：{error}")));
    if let Ok(mut pending) = DIRECTORY.lock() {
        *pending = Some(result);
    }
}

pub(super) fn take_directory_result() -> Option<DirectoryResult> {
    DIRECTORY.lock().ok().and_then(|mut pending| pending.take())
}

pub fn pick_file() {
    let app = bevy::android::ANDROID_APP
        .get()
        .expect("Android initialized");
    let result = (|| -> jni::errors::Result<()> {
        // The VM and activity pointers are owned by AndroidApp for its lifetime.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast())? };
        let mut env = vm.attach_current_thread()?;
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        env.call_method(&activity, "pickFile", "()V", &[])?;
        Ok(())
    })();
    if let Err(error) = result {
        error!("Android file picker: {error}");
        if let Ok(mut picked) = PICKED.lock() {
            *picked = Some(None);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_bevyengine_portalo_MainActivity_nativeFilePicked(
    mut env: JNIEnv,
    _activity: JObject,
    path: JString,
) {
    if let Ok(path) = env.get_string(&path) {
        let path = String::from(path);
        if let Ok(mut picked) = PICKED.lock() {
            *picked = Some(if path.is_empty() {
                None
            } else {
                Some(PathBuf::from(path))
            });
        }
    }
}

pub(super) fn poll_file(
    mut messages: MessageWriter<super::PickedFile>,
    mut intents: MessageWriter<super::Intent>,
    state: Res<State<portalo_core::AppState>>,
) {
    if BACK.swap(false, Ordering::Relaxed) {
        if *state.get() == portalo_core::AppState::Menu {
            let _ = activity_method("backgroundApp", None);
        } else {
            intents.write(super::Intent(super::Action::Back));
        }
    }
    if let Some(path) = PICKED.lock().ok().and_then(|mut value| value.take()) {
        messages.write(super::PickedFile { path });
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_bevyengine_portalo_MainActivity_nativeBackRequested(
    _env: JNIEnv,
    _activity: JObject,
) {
    BACK.store(true, Ordering::Relaxed);
}

fn activity_method(method: &str, text: Option<&str>) -> jni::errors::Result<()> {
    let app = bevy::android::ANDROID_APP
        .get()
        .expect("Android initialized");
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast())? };
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    if let Some(text) = text {
        let text = env.new_string(text)?;
        env.call_method(
            activity,
            method,
            "(Ljava/lang/String;)V",
            &[jni::objects::JValue::Object(&text)],
        )?;
    } else {
        env.call_method(activity, method, "()V", &[])?;
    }
    Ok(())
}

pub fn copy_text(text: &str) -> jni::errors::Result<()> {
    activity_method("copyText", Some(text))
}
