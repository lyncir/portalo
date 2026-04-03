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
