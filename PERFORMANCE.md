# 性能验证与传输协议

## 已实现

- 协议 v1：`PORTALO1`（8 字节）、文件名长度（u32 LE）、UTF-8 文件名、文件大小（u64 LE）、文件内容、接收方 `RECEIVED`（8 字节）确认。两端必须同时升级。
- 完成确认表示接收方收到精确字节数、flush 并关闭文件、发布最终文件；不代表执行 fsync 后的断电持久化。截断不会被报成功。
- 默认每任务 256 KiB 复用缓冲；`PORTALO_BUFFER_KIB=64/256/1024` 可调整，两端可分别设置。不是内核零拷贝。
- 接收同时最多 2 个任务，超出的连接立即关闭；UI 一次发送一个任务。连接每地址最多 2 秒、累计约 10 秒；协议头、每次 I/O 和 ACK 等待最多 30 秒。
- 临时文件接收，同名文件追加 `.1`、`.2` 等后缀。优先通过 tempfile 的平台原生无覆盖重命名发布；旧内核按该库策略回退到硬链接。Android 模拟存储另外提供独占预留名称后重命名的兼容路径，具体语义见 `ANDROID_VALIDATION.md`。
- 进度为容量固定的 watch 快照，最多约 10 次/秒。UI 按 socket 写入字节统计，最终成功以接收确认判定。单发送任务的终态保留至下次任务，无历史进度队列。
- 设备按服务实例跟踪，按主机名及端口聚合地址。纯心跳不触发设备列表重建；删除一个接口不删除其他接口。网卡枚举放在 I/O 线程池，心跳使用缓存结果。
- 前台最长约 100 ms、失焦最长约 1 s 定时刷新，输入可立即唤醒；发现计时使用真实时间，传输在 Tokio 中独立运行。

## Android

默认接收目录是应用内部数据目录下 `downloads`，不是公共 Downloads。桌面默认 `./downloads`，可通过 `PORTALO_DOWNLOAD_DIR` 覆盖。

安卓也可在设置中通过系统选择器选择接收文件夹。SAF 接收直接使用文档文件描述符流式写入，不复制整份缓存；协议确认在写入、关闭和重命名后发送，详情与实测见 `SAF_RECEIVE.md`。

系统文件选择器通过 SAF 选择 `content://` 文件，在后台以 256 KiB 缓冲流式复制到应用缓存，再交给 Rust 发送；不会把整个文件加载进内存，发送后删除缓存。**选择到发送之间有额外本地复制时间和磁盘空间需求**，测速需把这部分单独记录。跨 provider 的直接文件描述符传输尚未实现。

多播锁在 Activity 可见时持有，停止或销毁时释放。Android 暂停/杀进程、Doze 和后台限制仍需真机验证；当前没有前台传输 Service，不承诺长时间后台可靠传输。

```sh
cargo ndk -t arm64-v8a -P 26 -o android_app/src/main/jniLibs build --release --package portalo --lib
# 在 android_app 中运行
bash gradlew assembleDebug
```

Gradle 是单根 application 项目，默认 ABI 为 arm64-v8a。上述 debug APK 包装的是 release Rust 库，Java 调试开关不改变 Rust 优化等级。

## 本地可重复测试

```sh
cargo test --workspace --lib
cargo check --workspace --all-targets
cargo ndk -t arm64-v8a -P 26 check --workspace
cargo build --release -p portalo_network --example benchmark --example transfer
PORTALO_BUFFER_KIB=64 target/release/examples/benchmark
PORTALO_BUFFER_KIB=256 target/release/examples/benchmark
PORTALO_BUFFER_KIB=1024 target/release/examples/benchmark
```

benchmark 每轮传输 64 MiB，共 5 次，计时包括接收确认，计时外逐字节验证，临时文件自动清理。它是热缓存、回环网络测试，不能代表 Wi-Fi、安卓闪存或冷盘吞吐，也不是旧版本对照数据。

## 两台设备测试

```sh
# 接收端（与 GUI 二选一，避免同一端口冲突）
PORTALO_BUFFER_KIB=256 target/release/examples/transfer receive ./bench-downloads
# 发送端，替换 IP 和真实文件路径
PORTALO_BUFFER_KIB=256 target/release/examples/transfer send 192.168.1.2:49527 /path/to/file
```

使用空文件、4 KiB、256 MiB、2 GiB 文件，双向各运行至少 5 次，记录中位数与最慢一次、CPU、RSS、连接耗时、接收确认耗时；对发送与接收文件执行 SHA-256 比对。区分冷/热缓存，并使用相同网络、存储、后台负载和电量条件。

体积优化对照：`cargo build --profile release-small -p portalo_network --example benchmark`，运行 `target/release-small/examples/benchmark`。正式吞吐优化采用 release `opt-level=3`，保留 thin LTO；参数的设备最优值需由数据决定。

## 本次本机结果

64 MiB × 5 次，release 回环热缓存测试，逐字节验证全部通过：

| 缓冲 | 中位数 MiB/s |
| --- | ---: |
| 64 KiB | 2191.13 |
| 256 KiB | 2686.31 |
| 1024 KiB | 2732.79 |

同样 256 KiB 缓冲、`release-small` 中位数为 2658.23 MiB/s；与 release 差异很小，本次样本**不支持宣称 opt-level=3 提高传输吞吐**。以上为最终实现的复测结果。保留 256 KiB 作为手机/桌面的折中默认值，1 MiB 在本机回环略快，真机最优值待测。这些数值不是 Wi-Fi 实测速率，也不是对旧版的提速百分比。

上述回环测试之后，已连接 Sony J9110 完成双向传输、SHA-256 校验、内部/外部目录及短时后台验证；单次真机数据与验证范围见 `ANDROID_VALIDATION.md`。耗电和持续吞吐仍未测量。

已通过：8 项 Rust 回归测试、桌面所有 targets 的 Clippy（`-D warnings`）、Android arm64 Clippy（`-D warnings`）、Android arm64 Rust release 链接构建及 Gradle debug APK 构建。APK 内包含 release Rust 库。

检查断网、截断、ACK 丢失、多网卡失效地址、同名并发、连续任务、前后台切换。ACK 丢失时接收文件可能已发布而发送方显示失败，重试产生带后缀副本，这是没有幂等重传协议时的明确语义。
