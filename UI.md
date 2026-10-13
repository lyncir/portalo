# Portalo UI

## 设计与页面

界面参考 Dukto R6 的 Metro 风格，采用浅灰底、大标题、直角主题色设备块和线性设备图形。
默认主题色为橙色，可在设置中切换为蓝色或绿色。

- **附近设备**：本机信息、自动发现的设备、空列表提示；设备按名称排序，地址变化只更新文本，保留实体及焦点。
- **发送文件**：先确认目标设备，再选择文件；桌面支持将单个文件拖入该页。批量拖入会提示逐个发送，文件夹会明确报错。
- **文件传输**：显示文件名、目标、进度、已发送大小、速度和剩余时间。数据写完后仍显示“等待对方确认”，收到 ACK 才显示绿色完成状态；失败可展开详情。
- **连接信息**：本机名称、IPv4 地址、端口、协议说明，以及复制地址。
- **设置**：持久化主题色和接收目录，提供恢复默认目录及离线字体许可。

桌面初始逻辑尺寸为 760 × 640，支持缩小至 320px 宽。各页都有滚动区域与位置指示，长设备名、路径和错误文本按字符回退换行。安卓不再强制隐藏系统栏。

## 操作

- 鼠标点击 / 触摸轻点；按钮有悬停、按下、焦点和禁用样式。
- `Tab` / `Shift+Tab` 切换焦点，`Enter` / `Space` 执行；焦点移出可见区域时自动滚动。
- 鼠标滚轮、触摸拖动、`PageUp` / `PageDown` 滚动。
- `Esc` 返回；安卓系统返回键在子页面返回，在主页将应用放到后台。
- 文件选择/准备期间禁用重复选择；传输期间返回按钮禁用，直到成功、失败或超时。没有伪装成可用操作的取消按钮。

按钮保留 Bevy 的可访问性 Button 角色，但使用直接点击和键盘事件派发，避免低频更新时实验性 Pressed 组件的延迟写入导致快速点击失效。拖动滚动时抑制点击操作。

## 配置与接收目录

Linux 配置位于 `$XDG_CONFIG_HOME/portalo/settings.json`，未设置 XDG 路径时使用 `~/.config/portalo/settings.json`。
Windows 使用 `%APPDATA%/portalo/settings.json`。安卓保存在应用内部数据目录。

配置写入和目录可写性检查运行在 I/O 工作线程上。只有保存成功才更新运行时设置；保存失败保留旧配置并显示错误。
接收服务在**每个连接建立时**读取当前目录，因此设置更改立即影响后续接收，进行中的接收继续使用原目录。
`PORTALO_DOWNLOAD_DIR` 在启动时覆盖已保存的目录；如需重启后仍使用 UI 设置，请移除此环境变量。

桌面使用系统目录选择器。安卓现在支持通过 SAF 系统选择器选择接收文件夹，并持久保存目录授权；同时保留内部目录和应用外部专属目录。使用步骤、Android 目录限制、存储语义及真机测试见 `SAF_RECEIVE.md`。

## 字体和性能

文泉驿微米黑作为内嵌字体，覆盖常见拉丁字符及 CJK；Linux 可执行文件无需旁边放置字体文件。原始字体和 Apache 2.0 许可位于 `crates/ui/assets/`，许可也嵌入应用的“字体许可”页。

UI 保留 100ms 前台 / 1s 失焦定时唤醒策略，用户输入会唤醒窗口。进度使用最新快照，显示文本只在状态变化时重新格式化；设备条目按变化更新；页面通过 `DespawnOnExit` 清理。主题更新仅修改实际变化的样式，没有常驻动画。

## 构建

```sh
cargo build --release --bin portalo
./target/release/portalo

cargo ndk -t arm64-v8a -P 26 -o android_app/src/main/jniLibs build --release --package portalo --lib
# 在 android_app 目录
bash gradlew assembleDebug
```

Linux 程序：`target/release/portalo`。
Android APK：`android_app/build/outputs/apk/debug/Portalo-debug.apk`（内含 release Rust 库）。

## 验证

```sh
cargo test --workspace --lib
cargo clippy --workspace --all-targets -- -D warnings
cargo ndk -t arm64-v8a -P 26 clippy --workspace -- -D warnings
```

UI 回归覆盖快速点击、禁用/拖动防误触、跨帧批量拖放、设备条目复用、页面反复进入清理、准备/传输期间导航限制、ACK 前后返回按钮状态及测速。
配置测试覆盖中文目录往返保存和无效目录的失败回滚。

已在 Linux X11 实际渲染并检查桌面与窄屏截图、键盘导航、设置页滚动、剪贴板反馈、主题切换及保存；使用独立临时测试程序和模拟接收端检查设备、发送、等待 ACK 和成功页面。
Android 11 / Sony J9110 已完成真机验证并修复触摸、返回键、设备名称、系统安全边距和外部存储问题，详见 `ANDROID_VALIDATION.md`。其他机型、云盘 provider 和长时间后台仍需进一步验证。
