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

pub fn pick_directory() -> io::Result<()> {
    activity(|env, object| {
        env.call_method(object, "pickReceiveDirectory", "()V", &[])?;
        Ok(())
    })
}

pub fn begin(uri: &str, name: &str) -> io::Result<i64> {
    activity(|env, object| {
        let uri = env.new_string(uri)?;
        let name = env.new_string(name)?;
        env.call_method(
            object,
            "beginReceiveDocument",
            "(Ljava/lang/String;Ljava/lang/String;)J",
            &[JValue::Object(&uri), JValue::Object(&name)],
        )?
        .j()
    })
}

pub fn duplicate_fd(token: i64) -> io::Result<i32> {
    activity(|env, object| {
        env.call_method(object, "receiveDocumentFd", "(J)I", &[JValue::Long(token)])?
            .i()
    })
}

pub fn finish(token: i64) -> io::Result<()> {
    activity(|env, object| {
        env.call_method(
            object,
            "finishReceiveDocument",
            "(J)V",
            &[JValue::Long(token)],
        )?;
        Ok(())
    })
}

pub fn abort(token: i64) -> io::Result<()> {
    activity(|env, object| {
        env.call_method(
            object,
            "abortReceiveDocument",
            "(J)V",
            &[JValue::Long(token)],
        )?;
        Ok(())
    })
}
