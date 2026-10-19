# Bevy 0.20 升级

项目从 Bevy 0.18.1 升级到 0.20.0。Rust 最低版本为 1.97.1，开发环境和三平台 CI 均固定该版本；依赖解析结果记录在 `Cargo.lock` 中。

## 新版能力与迁移

- **保留式 UI 渲染**：使用 0.20 的默认 UI 渲染后端。接收卡片仍复用实体并仅在内容变化时修改组件，以配合新版的增量更新机制；未做性能基准对比。
- **相对字号与间距**：公共文字使用 `FontSize::Rem`，页面间距和按钮尺寸使用 `rem()`，通过 `RemSize(16.0)` 统一基准。当前默认大小保持原来的逻辑像素尺寸。
- **字体与排版**：迁移到 `Font::from_bytes`、`TextLayout::linebreak/no_wrap`，继续内嵌文泉驿中文字体。
- **扁平指针事件**：使用 `PointerClick`、`PointerPress`、`PointerDrag` 等新版事件。
- **输入焦点**：通过 `InputFocus::get/set/clear` 访问焦点，点击明确标记为 `FocusCause::Pressed`，保留新版焦点变化通知。
- **按钮状态**：用 `Hovered` 和原生 `Pressed` 组件替换已弃用的 `Interaction`；按下、松开、取消与拖动结束状态由新版 `ButtonPlugin` 管理。
- **变换裁剪**：触摸命中检测使用 `CalculatedClip::contains_point`，正确处理局部坐标裁剪；层叠优先级读取独立的 `ComputedStackIndex`。
- **插件注册**：新版 `DefaultPlugins` 已包含输入分发和 UI 控件插件，UI 初始化避免重复添加。
- **布局适配**：根页面和内容列显式设置零最小高度，滚动视口使用零 flex basis，防止内容最小尺寸把接收卡片挤出窗口。按钮文字指定可用宽度并居中，避免新版文本测量导致中文逐字换行。

低功耗事件循环中的同帧按下/松开，以及安卓原始触摸兜底仍直接派发操作，不依赖延迟插入的 `Pressed` 判断点击。该路径与原生 `Activate` 不重复绑定，避免一次操作触发两次发送。

## 桌面文件选择

`bevy_file_dialog 0.9.1` 依赖 Bevy 0.18，目前没有兼容 0.20 的已发布版本。改为直接使用 `rfd::AsyncFileDialog`，在 Bevy I/O 任务池上执行并轮询结果，支持文件和接收目录选择、取消后恢复控件状态。

Android 继续使用现有 JNI/SAF 选择与持久授权路径。

## 本次验证

- 28 项 Rust 回归测试通过；触摸测试覆盖新版局部坐标裁剪及焦点设置。
- Linux、Android arm64 严格 Clippy 通过，Windows MSVC 交叉检查通过。
- Linux 实际窗口验证中文排版、两张接收卡片、键盘 Tab/Enter 导航、原生目录对话框和取消后的状态恢复。
- Linux 两路限速接收完成，输出文件 SHA-256 与发送内容一致。
- Linux 和 Windows release 程序、Android arm64 release 原生库及 Debug APK 均已构建成功。
- Android 11 / Sony J9110 真机验证通过，详情见下节；未做 Windows 本机运行验证。

### Android 真机补测（2026-10-19）

- 成功安装并冷启动，中文、绿色主题、系统栏留白、按钮及两张接收卡片显示正常。
- SAF `Dukto` 目录：6 MiB + 4 MiB 并发限速接收，分别收到 ACK，SHA-256 一致。一条流暂停 3 秒时显示零速，另一条继续更新。
- 接收期间触摸进入设置、系统 BACK 返回，卡片和传输持续正常；完成显示耗时及平均速度。
- 空文件正常保存；截断连接显示失败原因且不发布残缺文件；四条结束通知可逐条关闭。
- 强制停止后冷启动，原 SAF 授权仍有效，成功接收并核验新的小文件。
- mDNS 发现 Linux 端；通过系统选择器手动选择 8 MiB 测试文件发送到电脑，手机显示“对方已确认接收”，电脑侧 SHA-256 一致。系统选择器未响应 ADB 文件点击，实际选择由用户完成。
- 未观察到崩溃、Rust panic 或渲染错误。初次启动日志有 Gilrs 不支持 Android 的日志，另有主动截断测试产生的预期接收错误。
- 主题及目录设置逐字节核对保持不变；手机上的测试文件已清理。

手机旧版使用本机调试签名，与仓库固定的 CI 测试签名不同。本次将构建结果复制到 `/tmp/opencode/portalo-bevy20-device.apk`，使用匹配旧版的本机调试密钥重新签名后覆盖安装，保留应用数据和授权；仓库签名配置及标准 APK 未修改。

## 检查命令

```sh
cargo test --locked --workspace --lib
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo ndk -t arm64-v8a -P 26 clippy --locked --workspace -- -D warnings
cargo xwin check --locked --target x86_64-pc-windows-msvc --workspace
cargo build --locked --release --bin portalo
cargo ndk -t arm64-v8a -P 26 -o android_app/src/main/jniLibs build --locked --release --package portalo --lib
# 在 android_app 目录下
bash gradlew assembleDebug
```

上游迁移指南：
- https://bevy.org/learn/migration-guides/0-18-to-0-19/
- https://bevy.org/learn/migration-guides/0-19-to-0-20/
