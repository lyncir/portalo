support


version

1. rust 1.89 或更新版本（Bevy 0.18.1 的最低要求）
2. bevy 0.18.1


开发时运行::

	cargo run --features bevy/dynamic_linking


linux编译windows命令::

	# 安装 xwin 辅助工具
	cargo install cargo-xwin
	# 安装 Windows 目标平台
	rustup target add x86_64-pc-windows-msvc
	# 编译
	cargo xwin build --release --target x86_64-pc-windows-msvc


linux编译android命令::

	rustup target add aarch64-linux-android x86_64-linux-android
	cargo install cargo-ndk

	# 安装sdk ndk
	sdkmanager "platforms;android-26" "platform-tools" "ndk;27.0.12077973"

	cargo ndk -t arm64-v8a -P 26 -o android_app/src/main/jniLibs build --release --package portalo --lib
	# 在 android_app 目录运行（根项目就是 Android application）
	bash gradlew assembleDebug
	# 安装apk
	./gradlew installDebug
	# 调试
	adb logcat | grep 'RustStdoutStderr\|bevy\|wgpu'

性能测试与新协议说明见 PERFORMANCE.md。新旧协议不兼容，请同时更新两端。

Metro 风格 UI、快捷键、接收目录设置和构建产物说明见 UI.md。

安卓“选择接收文件夹”的使用方法与 SAF 真机验证见 SAF_RECEIVE.md。

Linux 桌面发行版构建与运行::

	cargo build --release --bin portalo
	./target/release/portalo
