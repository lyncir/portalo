support


version

1. Rust 1.97.1（rust-toolchain.toml 与 CI 固定相同版本）
2. Bevy 0.20.0

升级细节与新版 UI/输入能力见 BEVY_UPGRADE.md。


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

Linux/Android 的 sendfile/splice 快速路径、SAF 文档直传及回退机制见 ZERO_COPY.md。

Metro 风格 UI、快捷键、接收目录设置和构建产物说明见 UI.md。

安卓“选择接收文件夹”的使用方法与 SAF 真机验证见 SAF_RECEIVE.md。

应用内接收通知、进度与速度统计说明见 RECEIVE_NOTIFICATIONS.md。

Linux 桌面发行版构建与运行::

	cargo build --release --bin portalo
	./target/release/portalo

GitHub 自动构建与发行
--------------------

推送 master/main、提交 Pull Request 或手动运行 Build and Package 工作流，
会在 ubuntu-latest 上构建 Linux x86_64、Windows x86_64（cargo-xwin）和
Android arm64-v8a Debug APK。推送 v* 标签时，三端全部成功后自动发布 GitHub Release。
依赖通过仓库中的 Cargo.lock 与 --locked 锁定；Rust 固定为 1.97.1，构建工具仍随上游更新。

Linux 压缩包解压后运行 ``bash install.sh``，默认安装到 ~/.local；
使用 root 运行则安装到 /usr/local。用户级桌面入口和图标遵循 XDG_DATA_HOME
（默认 ~/.local/share）。桌面入口使用绝对路径，不依赖 PATH。
终端运行 portalo 需将 ~/.local/bin 加入 PATH，也可直接运行 ~/.local/bin/portalo。
该包是在 ubuntu-latest 上动态链接构建的普通二进制，不是通用 AppImage；
运行系统需要兼容的 glibc、图形驱动及系统库。可通过 ``ldd portalo`` 检查缺少的库。

Android APK 使用 release 优化的 Rust 库和 Gradle Debug 构建。
android_app/debug.keystore 是公开的固定测试签名（密码 android），仅用于 Debug 包；
后续使用该密钥构建的包可以相互覆盖安装。之前使用其他密钥签名的 APK 无法直接
覆盖安装，迁移前请备份所需数据；卸载旧应用会清除其应用数据。
该密钥不用于正式 Release 签名。
