# Portalo

局域网文件传输工具。参考 Dukto R6 的 Metro 风格界面，支持 Linux、Windows 和 Android。

## 功能

- 自动发现局域网内的其他 Portalo 设备（mDNS）
- 支持同时收发，接收进度实时显示在页面底部
- Android 通过 SAF 系统选择器选择接收文件夹并持久授权
- Linux/Android 发送与接收使用 `sendfile`/`splice` 内核传输路径

## 使用

### 界面与操作

- **附近设备**：自动列出局域网内的其他 Portalo。点击设备进入发送页。
- **发送文件**：选择目标设备后，点击"选择文件"（桌面也支持拖入单个文件）。
- **文件传输**：显示进度、速度、剩余时间；数据传完后等待对方确认，收到 ACK 才显示完成。
- **连接信息**：本机名称、IP 地址、端口及协议说明。
- **设置**：切换主题色（橙/蓝/绿），设置接收目录。

键盘：`Tab`/`Shift+Tab` 切换焦点，`Enter`/`Space` 执行，`Esc`/系统返回键后退，`PageUp`/`PageDown` 滚动。

### 接收通知

接收任务显示在页面底部，不切换页面，可以边浏览边接收。

- 最多同时两张进度卡片，最多保留 8 条历史结果。
- 状态：**正在接收 → 正在保存 → 文件已保存，正在确认 → 接收完成**。
- 到达 100% 不等于完成，仍须保存并写回 ACK。
- 关闭卡片只移除通知，不删除已保存的文件。

### 配置与接收目录

| 平台 | 配置文件 |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/portalo/settings.json`（默认 `~/.config/portalo/`） |
| Windows | `%APPDATA%\portalo\settings.json` |
| Android | 应用内部数据目录 |

`PORTALO_DOWNLOAD_DIR` 环境变量在启动时覆盖已保存的接收目录；移除该变量后 UI 设置恢复生效。

接收服务在每个连接建立时读取当前目录，设置更改立即对后续接收生效，正在进行的接收继续使用原目录。

### Android SAF 接收文件夹

1. 打开 Portalo → **设置 → 选择接收文件夹**。
2. 在系统选择器中进入目标目录，点击"使用此文件夹"并允许读写。
3. Portalo 校验写入、重命名能力；成功后设置页显示文件夹名称和 URI。
4. 授权持久化，重启应用后无需再次授权。

Android 11+ 通常禁止选择存储根、`Download` 根、`Android/data` 等位置，需选择允许的子目录。
授权撤销或目录删除后接收会明确失败并提示，不会静默回退到内部目录。

## 构建

工具链：Rust **1.97.1**（`rust-toolchain.toml` 与 CI 固定）、Bevy **0.20.0**。

### Linux

```sh
# 开发运行
cargo run --features bevy/dynamic_linking

# Release 构建
cargo build --release --bin portalo
./target/release/portalo
```

### Windows（从 Linux 交叉编译）

```sh
cargo install cargo-xwin
rustup target add x86_64-pc-windows-msvc
cargo xwin build --release --target x86_64-pc-windows-msvc
```

### Android

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk
# 安装 SDK/NDK（按需替换版本）
sdkmanager "platforms;android-26" "platform-tools" "ndk;27.0.12077973"

# 构建 Rust 库
cargo ndk -t arm64-v8a -P 26 -o android_app/src/main/jniLibs build --release --package portalo --lib

# 在 android_app 目录打包和安装
cd android_app
bash gradlew assembleDebug
./gradlew installDebug

# 查看日志
adb logcat | grep 'RustStdoutStderr\|bevy\|wgpu'
```

产物：`android_app/build/outputs/apk/debug/Portalo-debug.apk`（内含 release 优化的 Rust 库）。

## 验证

```sh
cargo test --locked --workspace --lib
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo ndk -t arm64-v8a -P 26 clippy --locked --workspace -- -D warnings
cargo xwin check --locked --target x86_64-pc-windows-msvc --workspace
```

## CI 与发布

推送 `master`/`main`、提交 PR 或手动触发工作流，在 `ubuntu-latest` 上构建 Linux x86_64、Windows x86_64 和 Android arm64 Debug APK。推送 `v*` 标签时，三端全部成功后自动发布 GitHub Release。

依赖通过 `Cargo.lock` 与 `--locked` 锁定；Rust 固定为 1.97.1。

## Linux 安装包

解压后运行 `bash install.sh`，默认安装到 `~/.local`；以 root 运行则安装到 `/usr/local`。

```sh
# 终端直接运行
~/.local/bin/portalo
```

构建产物动态链接 glibc，不是 AppImage；运行时需要兼容的 glibc、图形驱动和系统库。可用 `ldd portalo` 检查缺少的库。

## Android APK 签名

`android_app/debug.keystore` 是仓库内的固定测试签名（密码 `android`），仅用于 Debug 包。
使用该密钥构建的 APK 可相互覆盖安装；之前使用其他密钥签名的旧版本无法直接覆盖，迁移前请备份数据。

## 协议

TCP 端口 **49527**，不兼容旧版本（`PORTALO1`），请同时更新两端。

实现细节、零拷贝路径、SAF 存储语义和基准数据见 [INTERNALS.md](INTERNALS.md)。
