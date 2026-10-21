# Portalo 内部实现说明

面向开发者和贡献者。用户使用文档见 [README.md](README.md)。

## 传输协议

TCP 端口 **49527**，字节序均为小端，UTF-8 编码。

```
[魔数 8 B] "PORTALO1"
[文件名长度 4 B] u32 LE，不含终止符，最大 4096 字节
[文件名] UTF-8，不含路径分隔符和 NUL
[文件大小 8 B] u64 LE，精确值
[有效载荷] 精确 size 字节
  ← [ACK 8 B] "RECEIVED"
```

- 接收方确认表示已收到精确字节数、flush、关闭文件并发布最终文件，不代表 fsync 或云端持久化。
- 截断连接不会被误报为成功。
- ACK 写出失败时接收文件已保存，发送方会显示失败；重试产生带 `.1`、`.2` 后缀的副本，这是没有幂等重传协议时的明确语义。
- 同时最多接受 2 个并发接收任务，超出连接立即关闭。
- 连接超时：每地址 2 秒、累计约 10 秒；协议头、每次 I/O 和 ACK 等待最多 30 秒。

**协议不向后兼容，两端必须同时升级。**

## 缓冲区与环境变量

| 变量 | 含义 | 默认 |
| --- | --- | --- |
| `PORTALO_BUFFER_KIB` | 回退缓冲区大小，64–1024 KiB | 256 |
| `PORTALO_DOWNLOAD_DIR` | 覆盖接收目录（启动时读取） | — |
| `PORTALO_DISABLE_ZERO_COPY` | 强制使用缓冲路径（不控制 Android Java 端） | — |

`PORTALO_BUFFER_KIB` 同时作为尽力申请的管道容量，内核拒绝增大时保留系统默认值。

## Linux/Android 零拷贝传输

"零拷贝"指有效载荷不经过应用用户态的中转缓冲区，不保证内核、驱动或文件系统内部没有复制。

| 平台 / 来源 | 发送 | 接收 |
| --- | --- | --- |
| Linux 普通文件 | `sendfile` | socket → pipe → 文件（`splice`） |
| Android 普通 SAF 文档（零偏移、已知大小） | 文档 FD → `sendfile`，无缓存副本 | 对普通文件 FD 使用 `splice` |
| Android 未知长度、非零偏移、管道或不可直读文档 | 后台缓存到唯一临时文件后发送 | 非普通文件 FD 保留带超时的 Tokio 流式写入 |
| Windows | 有界缓冲发送 | 有界缓冲接收 |

Windows `TransmitFile` 需要单独处理 overlapped 完成、取消和 socket 生命周期，暂不启用。

### 正确性与回退

- 每次系统调用限制传输长度，支持短读/短写、`EINTR`、`EAGAIN`；不会越过声明的文件大小读取下一帧。
- 快速路径遇到 `EINVAL`、`ENOSYS`、`EOPNOTSUPP`、`EXDEV`、`EPERM` 时切换到缓冲回退；磁盘满等真实 I/O 错误直接报告失败。
- `sendfile` 推进的字节偏移量等于成功发送的字节数，回退从该位置继续；接收端先排空已从 TCP 读入管道的字节，再切回缓冲读取 socket。
- 内核路径只在专用阻塞任务中访问文件，socket 始终非阻塞；`poll` 最多等待 100 ms 后检查取消和 30 秒无输出进展超时，不占用 Tokio 异步线程做磁盘调用。
- `sendfile` 不支持 `MSG_NOSIGNAL`：工作线程临时屏蔽 SIGPIPE，处理自己产生的信号后恢复原有掩码，不修改 Android/Java 进程的全局信号处理。
- 接收进度只统计已写入目标的字节，不把管道中尚未写出的字节计入完成。
- 本地接收在首次等待有效载荷前就持有临时路径清理对象，中止任务会清理残缺文件。
- 丢弃传输 future 会通知工作任务停止并关闭 FD，但无法中断已进入内核的磁盘/FUSE 调用，30 秒超时不是严格的存储截止时间。

### Android 发送源所有权（`SendSource`）

Java 在后台打开 `AssetFileDescriptor`。满足以下条件时移交 FD 所有权给 Rust，Java 关闭自己的描述符：
- 普通文件（`S_ISREG`）
- 起始偏移为零
- 长度已知且不大于底层文件大小

`SendSource::OpenFile` 只能被消费一次，避免克隆导致并发共享文件偏移。正常结束、失败、未采用或被新选择替换时均释放所有权。

缓存回退使用随机临时文件名，原始显示名称单独保存，避免相同名称的选择互相覆盖。缓存由引用计数清理对象管理，最后一个所有者释放后自动删除；普通用户文件路径不使用该删除语义。

### 诊断

Linux/Android 工作任务结束时输出一次 `INFO Payload copy`，包含 `direction`、`bytes`（实际传输量）和 `kernel_bytes`（经快速路径传输量）。

```sh
# 强制缓冲路径
PORTALO_DISABLE_ZERO_COPY=1 ./target/release/portalo

# 手动回环基准（热缓存，不含 fsync，仅对比本实现的两条路径）
cargo test --workspace --lib benchmark_kernel_and_buffered -- --ignored --nocapture
```

本机回环参考结果（64 MiB × 3 轮，调试版二进制）：
- 缓冲路径：2247–2324 MiB/s，CPU 45.9–47.4 ms
- 内核路径：1904–2039 MiB/s，CPU 42.9–45.6 ms

回环吞吐在内核路径下略低，CPU 小幅减少。Android 的直接收益是消除完整发送缓存的额外磁盘写入，这在回环基准中无法体现。

## Android SAF 接收存储实现

- 配置新增可选 `document_tree: { uri, label }`，旧配置无此字段时继续使用 `download_directory`。
- URI 与文件路径分开建模，不能把 `content://` 当成本地路径。
- Java 使用 `DocumentsContract` 创建唯一 `.portalo-<UUID>.part` 文档，通过 `ParcelFileDescriptor` 给 Rust 独立的文件描述符。Rust 直接流式写入，不先缓存整份文件，也不通过 JNI 逐块传递字节数组。
- 精确接收字节数、flush、关闭描述符后，查询已有文件名并重命名；应用内提交串行化，同名文件追加 `.1`、`.2` 后缀。成功后才写回 ACK。
- 断连和 I/O 失败关闭描述符并删除临时文档；删除失败明确报错；Activity 正常销毁也会清理打开的文档。
- SAF provider 必须支持创建、写入、重命名、删除；不满足时在选择验证或接收时明确拒绝。
- 重命名可见性、跨应用竞争和远端持久化由 provider 决定，不承诺文件系统级原子发布或云端落盘。
- 系统强杀、断电或 provider 失联可能遗留 `.part` 文件；没有崩溃恢复或断点续传。

### 运行 SAF instrumentation 测试

先在应用中选择名称以 `Portalo-SAF-` 开头的**专用空测试目录**，测试不操作普通用户接收目录。

```sh
cd android_app
bash gradlew assembleDebug assembleDebugAndroidTest
adb -s DEVICE install -r build/outputs/apk/debug/Portalo-debug.apk
adb -s DEVICE install -r build/outputs/apk/androidTest/debug/Portalo-debug-androidTest.apk
adb -s DEVICE shell am instrument -w -r \
  -e class org.bevyengine.portalo.SafStorageTest \
  org.bevyengine.portalo.test/androidx.test.runner.AndroidJUnitRunner
```

默认运行提交/同名/空文件、失败清理测试；`-e revokeTestTree true` 会撤销测试目录授权，之后需重新选择。

## 文件发布与同名处理

- 本地接收：`tempfile` 平台原生无覆盖重命名，旧内核回退到硬链接。
- Android 外部专属目录：独占创建空文件预留名称，再重命名替换预留文件（兼容拒绝 `NOREPLACE` 的模拟存储）。
- SAF：查询后重命名，串行化在 Rust 中完成。
- 同名文件追加 `.1`、`.2` 等后缀，不覆盖。

## 设备发现

- mDNS，设备按服务实例跟踪，按主机名及端口聚合多宿主地址。
- 纯心跳不触发设备列表重建；删除一个接口地址保留其他接口。
- 网卡枚举在 I/O 线程池，心跳使用缓存结果。
- Android 主机名使用机型加持久化应用随机后缀，不读取硬件标识。

## UI 架构要点

- Bevy 0.20.0，Rust 1.97.1。使用 `rfd::AsyncFileDialog`（在 Bevy I/O 任务池上执行）替代已停止维护的 `bevy_file_dialog`。
- 前台约 100 ms、失焦约 1 s 定时唤醒；用户输入立即唤醒。
- 接收进度使用容量固定的 watch 快照，约 10 次/秒更新；没有无界进度消息队列。
- 卡片复用 Bevy 实体，只在数据、速度采样、主题或页面挂载变化时修改组件。
- 按钮使用原始触摸/点击事件派发，避免低频更新下延迟写入的 `Pressed` 组件导致快速点击失效。

## 代码位置

| 模块 | 路径 |
| --- | --- |
| 传输协议、发送/接收循环 | `crates/network/src/lib.rs` |
| Linux/Android 内核传输 | `crates/network/src/kernel_copy.rs` |
| 发送源抽象 | `crates/network/src/source.rs` |
| 接收进度上报 | `crates/network/src/receive_progress.rs` |
| 接收任务账本 | `crates/core/src/receive.rs` |
| Android JNI/SAF 桥接（Rust 侧） | `crates/ui/src/android.rs` |
| Android SAF 存储（Java 侧） | `android_app/src/main/java/.../SafStorage.java` |
| 接收卡片与速度采样 | `crates/ui/src/receive.rs` |
| 发送流程与文件选择 | `crates/ui/src/send.rs` |
| 设置与配置 | `crates/core/src/settings.rs` |
