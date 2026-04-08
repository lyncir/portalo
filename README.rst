support


version

1. rust 1.88
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

	cargo ndk -t arm64-v8a -P 26 -o android_src/app/src/main/jniLibs build --package portalo
   	cd android_src && chmod +x gradlew && ./gradlew build && cd ..
