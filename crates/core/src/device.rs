/// Shared display/discovery identity. Android's Unix hostname is usually
/// "localhost", so use the model and an app-local persistent random suffix.
pub fn device_name() -> String {
    #[cfg(target_os = "android")]
    {
        use jni::{
            JavaVM,
            objects::{JObject, JString},
        };
        let result = (|| -> jni::errors::Result<String> {
            let app = bevy::android::ANDROID_APP
                .get()
                .expect("Android initialized");
            let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast())? };
            let mut env = vm.attach_current_thread()?;
            env.with_local_frame(8, |env| {
                let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
                let name = JString::from(
                    env.call_method(activity, "getDeviceName", "()Ljava/lang/String;", &[])?
                        .l()?,
                );
                Ok(env.get_string(&name)?.into())
            })
        })();
        result.unwrap_or_else(|_| "Android-device".into())
    }
    #[cfg(not(target_os = "android"))]
    {
        gethostname::gethostname().to_string_lossy().into_owned()
    }
}
