//! Blocking JNI operations: callers use an I/O worker, never Bevy's UI thread
//! for content-provider operations. Every call clears Java exceptions so an
//! invalid/revoked URI becomes an ordinary, actionable Rust I/O error.
use jni::{
    JNIEnv, JavaVM,
    objects::{JObject, JString, JValue},
};
use std::io;

fn activity<T>(
    call: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>,
) -> io::Result<T> {
    let app = bevy::android::ANDROID_APP
        .get()
        .ok_or_else(|| io::Error::other("Android 未初始化"))?;
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(io::Error::other)?;
    let mut env = vm.attach_current_thread().map_err(io::Error::other)?;
    let mut detail = None;
    let result = env.with_local_frame(16, |env| {
        let object = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let result = call(env, &object);
        if result.is_err() && env.exception_check().unwrap_or(false) {
            let exception = env.exception_occurred()?;
            env.exception_clear()?;
            if let Ok(value) = env.call_method(exception, "getMessage", "()Ljava/lang/String;", &[])
                && let Ok(value) = value.l()
                && !value.is_null()
                && let Ok(value) = env.get_string(&JString::from(value))
            {
                detail = Some(String::from(value));
            }
            if env.exception_check().unwrap_or(false) {
                env.exception_clear()?;
            }
        }
        result
    });
    result.map_err(|error| io::Error::other(detail.unwrap_or_else(|| error.to_string())))
}

pub fn validate_tree(uri: &str) -> io::Result<()> {
    activity(|env, object| {
        let uri = env.new_string(uri)?;
        env.call_method(
            object,
            "validateReceiveTree",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&uri)],
        )?;
        Ok(())
    })
}

pub fn reopen_send_document(uri: &str, offset: u64, size: u64) -> io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    let offset = i64::try_from(offset).map_err(io::Error::other)?;
    let size = i64::try_from(size).map_err(io::Error::other)?;
    let fd = activity(|env, object| {
        let uri = env.new_string(uri)?;
        env.call_method(
            object,
            "reopenSendDocument",
            "(Ljava/lang/String;JJ)I",
            &[
                JValue::Object(&uri),
                JValue::Long(offset),
                JValue::Long(size),
            ],
        )?
        .i()
    })?;
    if fd < 0 {
        return Err(io::Error::other("无法重新打开源文档，请检查授权"));
    }
    // Java detached this duplicate; Rust is its sole owner.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

pub fn pick_directory() -> io::Result<()> {
    activity(|env, object| {
        env.call_method(object, "pickReceiveDirectory", "()V", &[])?;
        Ok(())
    })
}

pub fn publish_transfer(tree: &str, path: &str, name: &str, id: &str) -> io::Result<String> {
    activity(|env, object| {
        let tree = env.new_string(tree)?;
        let path = env.new_string(path)?;
        let name = env.new_string(name)?;
        let id = env.new_string(id)?;
        let result=env.call_method(object,"publishTransfer","(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",&[JValue::Object(&tree),JValue::Object(&path),JValue::Object(&name),JValue::Object(&id)])?.l()?;
        Ok(env.get_string(&JString::from(result))?.into())
    })
}

pub fn transfer_save_progress(id: &str) -> io::Result<u64> {
    activity(|env, object| {
        let id = env.new_string(id)?;
        Ok(env
            .call_method(
                object,
                "transferSaveProgress",
                "(Ljava/lang/String;)J",
                &[JValue::Object(&id)],
            )?
            .j()? as u64)
    })
}

pub fn cancel_transfer(path: &str) -> io::Result<()> {
    activity(|env, object| {
        let path = env.new_string(path)?;
        env.call_method(
            object,
            "cancelTransfer",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&path)],
        )?;
        Ok(())
    })
}

pub fn prepare_transfer_target(tree: &str, state: &str, manifest: &str) -> io::Result<String> {
    activity(|env, object| {
        let tree = env.new_string(tree)?;
        let state = env.new_string(state)?;
        let manifest = env.new_string(manifest)?;
        let result = env
            .call_method(
                object,
                "prepareTransferTarget",
                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
                &[
                    JValue::Object(&tree),
                    JValue::Object(&state),
                    JValue::Object(&manifest),
                ],
            )?
            .l()?;
        Ok(env.get_string(&JString::from(result))?.into())
    })
}
pub fn open_transfer_document(uri: &str, write: bool) -> io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    let fd = activity(|env, object| {
        let uri = env.new_string(uri)?;
        env.call_method(
            object,
            "openTransferDocument",
            "(Ljava/lang/String;Z)I",
            &[JValue::Object(&uri), JValue::Bool(u8::from(write))],
        )?
        .i()
    })?;
    if fd < 0 {
        return Err(io::Error::other("Provider returned invalid descriptor"));
    }
    // SAFETY: Java detachFd transfers sole ownership of this new descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}
pub fn publish_transfer_target(state: &str) -> io::Result<String> {
    activity(|env, object| {
        let state = env.new_string(state)?;
        let result = env
            .call_method(
                object,
                "publishTransferTarget",
                "(Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(&state)],
            )?
            .l()?;
        Ok(env.get_string(&JString::from(result))?.into())
    })
}
pub fn cancel_transfer_target(state: &str) -> io::Result<()> {
    activity(|env, object| {
        let state = env.new_string(state)?;
        env.call_method(
            object,
            "cancelTransferTarget",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&state)],
        )?;
        Ok(())
    })
}
