# Portalo

局域网文件传输工具，支持 Linux、Windows 和 Android。界面参考 Dukto R6 的 Metro 风格。

- 自动发现同一局域网内的设备，可同时发送和接收。
- 发送单文件、多文件或整个文件夹，保留空目录。
- 分块校验，支持暂停、断线及重启后继续。
- Android 使用系统文件选择器（SAF），可指定接收文件夹。

## 快速使用

1. 在两台设备上打开 Portalo，确保连接同一局域网，并使用支持当前协议的版本。
2. 在 **附近设备** 中选择接收设备。
3. 点击 **选择文件并发送** 或 **选择文件夹并发送**。桌面端也支持拖入文件和文件夹。
4. 等待 **传输完成，对方已确认接收**。进度达到 100% 后，可能仍需保存和确认。

接收任务显示在页面底部，不切换当前页面。最多同时显示两张进度卡片，保留 8 条历史结果；关闭卡片不会删除文件。

### 暂停、继续与取消

- **暂停**：保留任务和已保存进度。
- **继续**：选择原接收设备，在发送页的“未完成任务”中点击继续；重启应用后也可恢复。
- **断线**：重新连接后由用户点击继续；少量未持久化数据可能需要重传。
- **取消**：需连接接收端确认清理。离线时保留任务，方便稍后清理；已发布的文件不会因取消被删除。

### 文件准备与保存规则

**传输完成前，请保留原文件且勿修改或移动。** 流式发送会边暂存、边校验、边发送，尚未暂存的部分仍依赖原文件。

| 来源 | 准备方式 |
| --- | --- |
| 桌面普通文件和目录中的文件 | 先建立清单，再分块准备，无需预先读取整个大文件 |
| Android 单选、可持久授权且支持定位读取的已知长度文件 | 分块准备；重启后可重新打开原文档 |
| Android 文件夹、多选、未知长度或不可重新打开的来源 | 先导出或缓存，再发送 |

- 发送端需预留所选内容大小的暂存空间；扫描或准备尚未建立任务时中断，需要重新选择。
- 单文件保存到接收目录；文件夹保留原名；多文件批量保存到任务目录。
- 同名目标追加 `.1`、`.2` 等后缀，续传沿用原目标。
- 完成记录保存在应用内部，不生成 `.portalo-receipt`，也不会自动清理用户已有文件或旧标记。
- 不跟随符号链接；不支持的文件名、跨平台名称冲突及 `.portalo-` 保留前缀会明确报错。

### Android 接收文件夹

1. 打开 **设置 → 选择接收文件夹**。
2. 在系统选择器中进入目标目录，点击 **使用此文件夹** 并授权。
3. 验证成功后，设置页显示文件夹名称和 URI；重启后无需再次授权。

Android 11+ 通常禁止选择存储根、`Download` 根及 `Android/data` 等位置，请选择允许的子目录。授权撤销或目录删除后会明确报错。

支持的本机存储可直接接收到临时文档，完成后重命名；其他提供方可能需要内部暂存后再复制，需同时预留内部和目标存储空间。传输中可见 `.portalo-<任务ID>.part`，完成后消失。

### 配置与快捷键

| 平台 | 配置文件 |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/portalo/settings.json`，默认 `~/.config/portalo/settings.json` |
| Windows | `%APPDATA%\portalo\settings.json` |
| Android | 应用内部数据目录中的 `settings.json` |

设置页支持主题色和接收目录。`PORTALO_DOWNLOAD_DIR` 在启动时覆盖已保存的接收目录，移除变量后恢复使用 UI 设置。目录更改对后续接收生效，已有任务沿用原目标。

快捷键：`Tab` / `Shift+Tab` 切换焦点，`Enter` / `Space` 执行，`Esc` / 系统返回键后退，`PageUp` / `PageDown` 滚动。

## 构建

Rust **1.97.1**（见 `rust-toolchain.toml`），Bevy **0.20.0**。以下命令从仓库根目录执行，依赖通过 `Cargo.lock` 和 `--locked` 固定。

### Linux x86_64

Debian / Ubuntu 构建依赖与 CI 一致：

```sh
sudo apt-get install g++ pkg-config libx11-dev libasound2-dev libudev-dev \
  libxkbcommon-dev libxkbcommon-x11-0 libwayland-dev

# 开发运行
cargo run --locked --features bevy/dynamic_linking

# Release
cargo build --locked --release --bin portalo
./target/release/portalo
```

### Windows x86_64（从 Linux 交叉编译）

```sh
sudo apt-get install clang llvm lld
cargo install cargo-xwin --locked
rustup target add x86_64-pc-windows-msvc
cargo xwin build --locked --release --target x86_64-pc-windows-msvc --bin portalo
```

产物：`target/x86_64-pc-windows-msvc/release/portalo.exe`。

### Android arm64

安装 JDK 17、Android SDK 命令行工具，并设置 `ANDROID_HOME`：

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
sdkmanager "platforms;android-34" "platform-tools" "build-tools;34.0.0" \
  "ndk;27.0.12077973" "cmake;3.22.1"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.0.12077973"

cargo ndk -t arm64-v8a -P 26 -o android_app/src/main/jniLibs \
  build --locked --release --package portalo --lib

cd android_app
bash gradlew assembleDebug
bash gradlew installDebug
```

产物：`android_app/build/outputs/apk/debug/Portalo-debug.apk`。这是包含 Release 优化 Rust 库的 Debug APK，最低支持 Android API 26。

仓库中的 `android_app/debug.keystore` 为固定测试签名（密码 `android`）。使用同一签名的 APK 可覆盖安装；不同签名无法直接覆盖，迁移前应备份数据。

## 检查与验证

```sh
cargo test --locked --workspace --lib
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo ndk -t arm64-v8a -P 26 clippy --locked --workspace -- -D warnings
cargo xwin clippy --locked --target x86_64-pc-windows-msvc --workspace -- -D warnings
```

移除旧协议后，62 项 Rust 单元测试、三端严格 Clippy 与 Release 构建通过，1 项手动基准默认忽略。Android 新版覆盖安装后通过 7 项真机自动化测试，以及 160 MiB 多文件传输、重启续传和发布恢复验证；Linux、Android 均验证拒绝旧协议。Windows 原生运行和 ReFS 尚待实机验证；具体检查状态与性能测量口径见 [内部实现与验证记录](INTERNALS.md#验证范围与实测记录)。

## 安装包与发布

- CI 在推送 `master` / `main`、提交 PR 或手动触发时构建 Linux、Windows 和 Android 包。
- 推送 `v*` 标签后，三端构建全部成功才发布 GitHub Release。
- Linux 安装包解压后运行 `bash install.sh`：普通用户安装到 `~/.local`，root 安装到 `/usr/local`。
- Linux 产物动态链接 glibc，并非 AppImage；可用 `ldd portalo` 检查缺少的运行库。

## 连接与协议

Portalo 使用 mDNS 自动发现设备，TCP 端口为 **49527**。找不到设备时，检查同网段连接、无线客户端隔离和防火墙设置。

收发均仅支持 **PORTALO2**，请同时更新两端。不再接受旧版协议连接。

协议、存储恢复、平台后端与诊断说明见 [INTERNALS.md](INTERNALS.md)。
