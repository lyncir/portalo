# Portalo

局域网文件传输工具。参考 Dukto R6 的 Metro 风格界面，支持 Linux、Windows 和 Android。

## 功能

- 自动发现局域网内的其他 Portalo 设备（mDNS）
- 支持同时收发，接收进度实时显示在页面底部
- Android 通过 SAF 系统选择器选择接收文件夹并持久授权
- 文件、多文件和整目录传输，包含空目录；单任务最多四条并行文件流
- 分块校验、暂停、断线/重启后继续，以及完成确认去重

## 使用

### 界面与操作

- **附近设备**：自动列出局域网内的其他 Portalo。点击设备进入发送页。
- **发送文件**：选择目标设备后，多选文件或点击“选择文件夹并发送”（桌面也支持拖入多个文件/目录）。
  发送时使用设备最新发现的地址；Android 文件准备显示已缓存大小，可取消，30 秒无进展会结束等待。
- **文件传输**：显示进度、速度、剩余时间；数据传完后等待对方确认，收到 ACK 才显示完成。
- **暂停/继续**：暂停保留检查点；重新进入发送页，在“未完成任务”中继续。重启应用后仍可恢复，需选择原接收设备。取消需连接接收端确认清理；离线时保留任务以便稍后清理。
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

### 可恢复传输的存储语义

- 普通文件先建立清单，再按 4 MiB 块暂存、校验并发送，无需先完整复制和哈希整个大文件。Linux/Android 优先 reflink，Windows 优先支持的块克隆；否则逐块复制。**传输完成前请保留原文件且勿修改**：尚未暂存的部分仍依赖原文件，变化会报错；已暂存块可稳定重传。仍需预留所选内容大小的空间，扫描阶段中断需重新选择。收发两端需更新到支持流式准备的版本。
- Android 已完成的私有导出缓存优先通过硬链接复用为任务快照，减少第二次缓存复制；原缓存清理不会删除任务数据。不支持硬链接时回退；用户原文件不采用这种复用方式。
- Android 单选文件若提供方支持持久 URI 授权、已知长度及定位读取，也按块暂存发送，重启后重新打开同一 URI；不再先复制整个大文件。无法持久重开、未知长度或管道来源仍先缓存；目录与多选仍需先导出。源被移动、修改或授权失效会报错，保留已捕获块。
- 每 4 MiB 校验；累计 32 MiB 或约 1 秒提交持久化检查点，暂停、文件结束和发布前也会提交。断线时尚未持久化的数据可能重传。续传由用户点击触发。
- 单文件直接保存到接收目录；文件夹保留原名；多文件批量使用任务目录。同名按 `.1`、`.2` 编号，续传不改变目标。
- 目录内容完整后才发布；不生成 `.portalo-receipt`，恢复和完成记录保存在应用内部。异常发布恢复严格核验完整目录结构与文件哈希，不兼容旧标记目录。
- 发送目录不会自动过滤旧标记；`.portalo-` 保留前缀按不支持的路径报错，普通隐藏文件正常发送。应用不会自动删除已有目录中的文件。
- Android SAF 新任务对本机外部存储提供方实际探测随机读写、截断、同步及文件/目录重命名能力；通过后直接写目标临时文档，结束时重命名，免去二次复制。其他提供方或不支持这些能力时保留内部暂存兼容路径，需要两份空间；旧任务保持原后端。传输期间可见 `.portalo-<任务ID>.part`，完成后消失。保存期间每 10 秒报告存活，兼容复制路径另报实际复制进度。
- Linux/Android 新协议发送载荷使用 `sendfile`；Windows 优先使用 `TransmitFile` 从系统文件缓存发送，不支持时回退至有界缓冲。分块校验仍需读取文件；接收端为校验使用有界缓冲，SAF 兼容路径的二次复制优先使用 `FileChannel.transferTo`，不是端到端零拷贝。
- 新端协商每文件最多两块在途数据，让网络与校验写入重叠；连接旧端使用一块窗口。活跃文件复用 FD，检查点在后台按批次同步，不持有任务级写入锁。Windows 缓冲发送按帧共享已核验数据块，减少逐帧分配和复制。
- 不跟随符号链接；跨平台名称冲突、Windows 保留名、不支持的路径和超限清单会明确报错。

本轮已通过 Rust 单元测试、三端严格 Clippy 检查及 Linux/Windows/Android Release 构建，Android 真机自动化测试通过。Windows 原生发送与 ReFS 尚未在 Windows 实机运行；详细验证范围见 [INTERNALS.md](INTERNALS.md)。

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

TCP 端口 **49527**，发送使用 **PORTALO2**，发现信息标记版本 2，请同时更新两端。接收器仍保留 PORTALO1 兼容入口，但旧协议没有续传和目录能力。

Windows 接收端会拒绝驱动器前缀、设备保留名、冒号及其他不受支持的文件名，避免特殊路径被当成普通文件保存。

实现细节、零拷贝路径、SAF 存储语义和基准数据见 [INTERNALS.md](INTERNALS.md)。
