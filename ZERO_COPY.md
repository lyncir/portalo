# 文件传输零拷贝优化

## 当前路径

| 平台 / 来源 | 发送 | 接收 |
| --- | --- | --- |
| Linux 普通文件 | `sendfile` | socket → pipe → 文件，使用 `splice` |
| Android 普通文件 / 可直接读取的 SAF 文档 | 文档 FD → `sendfile`，无需整文件缓存 | 对普通文件 FD 使用 `splice`，包括支持它的 SAF 提供方 |
| Android 未知长度、非零起始偏移、管道或不可直读文档 | 后台缓存到唯一临时文件，取得准确大小后发送 | 非普通文件 FD 保留带超时的 Tokio 流式写入 |
| Windows | 原有有界缓冲发送 | 原有有界缓冲接收 |

这里的零拷贝指有效载荷不经过应用的用户态中转缓冲区，不保证内核、驱动或文件系统内部完全没有复制。`TransmitFile` 需要单独处理 Windows overlapped 完成、取消和 socket 生命周期；当前没有用不可取消的阻塞调用替换现有 Windows 路径。

协议仍为 `PORTALO1`，大小、文件名和 `RECEIVED` ACK 均不变。

## 正确性与回退

- 每次系统调用限制传输长度，支持短读/短写、`EINTR`、`EAGAIN`；不会越过声明的文件大小读取下一帧。
- 快速路径遇到不支持的操作或策略限制（`EINVAL`、`ENOSYS`、`EOPNOTSUPP`、`EXDEV`、`EPERM`）时使用缓冲回退。磁盘满等真实 I/O 错误直接报告失败。
- `sendfile` 成功多少字节就推进多少文件偏移，回退从该位置继续。接收端先排空已经从 TCP 读进管道的字节，再开始缓冲读取 socket。
- 内核路径只在专用阻塞任务中访问文件，socket 始终保持非阻塞。`poll` 最多等待 100ms 后检查取消和 30 秒无输出进展超时，不占用 Tokio 异步工作线程进行磁盘系统调用。
- 丢弃传输 future 会通知工作任务停止并关闭其 FD。无法强行中断已经进入内核的磁盘/FUSE 调用，因此不能把这里的超时描述为严格的存储操作截止时间。
- `sendfile` 不支持 `MSG_NOSIGNAL`：工作线程临时屏蔽并处理自己产生的 SIGPIPE，再恢复原有掩码，不修改 Android/Java 进程的全局信号处理。
- 接收进度只统计已经写入目标的字节，不把管道中尚未写出的字节计入完成。异步侧每约 100ms 采样整数计数，终态同步最后计数。
- 文件发布仍使用原有不覆盖重命名/SAF 提交流程。达到 100% 后仍须完成保存、写回 ACK；没有新增 fsync 或云端持久化承诺。
- 本地接收在首次等待有效载荷前就持有临时路径清理对象，中止任务会清理残缺文件。

## Android 发送源所有权

Java 在后台打开 `AssetFileDescriptor`。普通文件、起始偏移为零、长度已知且不大于底层文件长度时，复制并移交 FD 所有权给 Rust；Java 关闭自己的描述符。

`SendSource` 保存显示名称和逻辑长度。打开的 FD 只能被消费一次，避免克隆消息导致并发共享文件偏移；正常结束、失败、未采用或被新选择替换时均会释放所有权。直接发送期间应保持原文件不变，文件缩短会报告失败。

缓存回退使用随机临时文件名，原始显示名称单独保存，避免相同名称的选择互相覆盖。缓存由引用计数清理对象管理，最后一个所有者释放后删除；普通用户文件不使用该删除语义。

## 诊断与对照

Linux/Android 工作任务结束时输出一次 `Payload copy`，包含方向、实际字节数 `bytes` 和经内核快速路径传输的 `kernel_bytes`。

桌面可强制使用缓冲路径：

```sh
PORTALO_DISABLE_ZERO_COPY=1 ./target/release/portalo
```

该环境变量只控制 Rust 的内核传输路径，不强制 Android Java 端生成缓存。`PORTALO_BUFFER_KIB` 继续控制回退缓冲区大小（64–1024 KiB，默认 256 KiB），并作为尽力申请的管道容量；内核拒绝增大时保留默认容量。

手动回环基准：

```sh
cargo test --workspace --lib benchmark_kernel_and_buffered -- --ignored --nocapture
```

基准每种模式发送 3 次 64 MiB，数据预热在页缓存中，计时不包含生成输入和输出校验，不调用 fsync；CPU 时间包含本进程的发送和接收线程。它对比的是本次实现的内核路径与缓冲回退，不是旧版本，也不是无线或磁盘吞吐基准。

本机最后一轮结果：缓冲路径 2247–2324 MiB/s、CPU 45.9–47.4ms；内核路径 1904–2039 MiB/s、CPU 42.9–45.6ms。此次回环吞吐降低，CPU 小幅减少，不能声称零拷贝一定更快。Android 已确认的直接收益是消除完整发送缓存及其额外磁盘写入；尚未测量进程峰值内存或不同提供方的性能。

最终检查：38 项 Rust 测试通过（另有 1 项手动基准通过），Linux/Android 严格 Clippy 通过，Linux、Windows、Android release 构建及 Debug APK 组装通过。Windows 未进行运行时实测。

## 真机验证（2026-10-21）

Sony J9110 / Android 11，保留绿色主题和 `Dukto` SAF 授权：

- 两个 8 MiB 文件并发限速接收，ACK 和 SHA-256 均正确。日志显示两路 `kernel_bytes=8388608`，确认 SAF 目标实际支持 `splice`。
- 暂停一路时该路显示零速，另一条持续更新；空文件和截断连接处理正常。
- 系统选择器由用户选择 64 MiB 文件；日志确认 `Selected document FD directly: 67108864 bytes`，发送 `kernel_bytes=67108864`，Linux 输出 SHA-256 一致。
- 发送期间采样的 `cache/outgoing` 始终为 8 KiB，没有产生 64 MiB 的缓存副本。
- 测试文件已从手机删除，设置逐字节核对保持不变。
