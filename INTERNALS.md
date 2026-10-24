# Portalo 内部实现说明

面向开发者与贡献者。使用、构建和安装说明见 [README.md](README.md)。收发均仅支持 PORTALO2；其他魔数在创建接收文件前即被拒绝。

## 代码导航

| 职责 | 路径 |
| --- | --- |
| 连接入口、协议验证 | `crates/network/src/lib.rs`、`connect.rs` |
| PORTALO2 模型、帧、收发状态机 | `crates/network/src/transfer/{model,wire,transport}.rs` |
| 来源准备、检查点与发布 | `crates/network/src/transfer/{preparation,storage}.rs` |
| 阶段统计与协议测试 | `crates/network/src/transfer/{metrics,tests}.rs` |
| Linux/Android 内核复制、Windows 原生后端 | `crates/network/src/{kernel_copy,windows_io}.rs` |
| 来源所有权、路径校验 | `crates/network/src/{source,filename}.rs` |
| 发现与重试 | `crates/discovery/src/lib.rs` |
| Android JNI 桥接 | `crates/core/src/android_storage.rs`、`crates/ui/src/android.rs` |
| Android 来源与存储 | `android_app/src/main/java/org/bevyengine/portalo/` 下的 `SendDocuments`、`TransferDocuments`、`DirectTransferStorage`、`SafStorage` |
| 发送、任务恢复、接收卡片 | `crates/ui/src/{send,transfers,receive,progress_bar}.rs` |
| 配置、接收任务账本 | `crates/core/src/{settings,receive}.rs` |

## PORTALO2 协议

### 帧与限制

每任务使用一个 TCP 连接，端口 49527，魔数为 8 字节 `PORTALO2`。所有多字节长度均为小端。

| 项目 | 规格 |
| --- | --- |
| 控制消息 | serde 外部标记枚举，UTF-8 JSON；前缀为 u32 字节长度，1–16 MiB |
| 清单 | JSON 最多 8 MiB，封装后仍须符合控制消息限制 |
| 数据帧 | `Data {file,offset}` 后跟 u32 载荷长度和 1–256 KiB 原始字节 |
| 校验块 | 4 MiB，尾块按实际长度；BLAKE3 |
| 并发 | 接收最多两任务，每任务最多四条文件流；目录不占流 |
| 在途窗口 | 每文件最多两块（8 MiB）；兼容缺省窗口为一块 |
| 超时 | 魔数等待 30 秒；控制帧及协议 I/O 等待 120 秒 |
| 连接尝试 | 地址去重后依次尝试，每地址 2 秒，总计 10 秒 |

消息示例：`{"Hello":{"version":2}}`；无字段消息编码为 `"Commit"`。流式清单使用 `streaming:true`，每块 `Data` 前发送 `Hash {file,offset,hash}`。

### 状态流转

1. `Hello {version:2}` → `Ready {streams,frame,block,window,streaming}`。接收任务满载返回 `Error {code:"BUSY",...}`。缺省 `streaming:false`；流式任务遇到不支持的端点要求更新。
2. `Begin {manifest,token}` → `Resume {offsets,result}`。已有完成记录时直接返回结果，否则返回重新核验后的持久偏移。
3. 每文件 `Open` → `Hash`（流式任务）/`Data` → `Checkpoint` → `Close` → `Closed`。所有块确认后才允许关闭该文件。
4. 所有文件关闭后 `Commit` → 可选 `Saving` / `SaveProgress` → `Complete {result}`。发送方收到 `Complete` 才成功。
5. `Pause` / `Cancel` → `Stopped`。暂停保留进度；取消等待在途工作结束后清理未发布内容，不删除已发布结果。

`Checkpoint.offset` 表示已核验写入并返还信用的位置，`durable` 表示已提交的持久位置。断线恢复始终以接收方 `Resume.offsets` 为准，不能凭发送方声明跳过数据。

恢复核验及提交期间每 10 秒发送 `Saving`；SAF 兼容复制路径另发 `SaveProgress`。这些消息避免长操作被误判为网络超时，但不证明提供方持续取得进展。进入提交后，即使连接断开，接收方仍完成发布并持久化结果，以供重连查询。

## 发送端流水线

### 清单与来源捕获

任务 ID 为 UUID，文件 ID 为清单索引，路径为 UTF-8 分量数组。清单限制 100000 项、64 层、每分量 240 字节；父目录先出现，拒绝重复、大小写冲突、特殊名称和 `.portalo-` 保留前缀，不跟随符号链接。

普通来源先固定路径和大小，再逐步建立不可变任务快照：

1. 首块最多 4 MiB，读取、校验来源身份并持久化后即可发送。
2. 后续每批最多捕获 32 MiB，复用来源句柄，批次前后检查大小、mtime 和文件身份；文档区域还校验 offset/length。
3. 先同步快照，再原子保存块摘要日志；只有已持久化的块可进入发送队列。
4. 后台提前准备下一批。未消费持久前缀达到 32 MiB 时等待，单批最多再追加 32 MiB；整批不驻留内存，捕获复用一个额外 4 MiB 临时缓冲。
5. 发送端不获取下一批写入锁，读取已提交块并复核哈希。退出时终止预取任务，等待不可中断捕获工作排空后再清理快照。

已捕获块可脱离原文件重传；未捕获部分仍依赖来源。元数据检查不提供对恶意还原时间戳等并发修改的强一致性保证，目录也不是瞬时时间点快照。完整捕获后移除来源依赖。

Linux/Android 优先尝试 reflink，Windows 使用支持的块克隆；不支持时复制。已拥有完整内容的快照按需计算哈希。Android 私有导出缓存可通过硬链接复用，普通用户文件不采用这种复用方式。

流式清单的 `blocks` 为空，哈希随块发送；原完整快照任务仍在清单内保存摘要。清单摘要为序列化结果的 BLAKE3，`streaming:false` 等兼容字段省略时保持旧摘要不变。空文件无校验块，空目录无载荷。

### 有界队列与流控

发送帧队列容量为 `STREAMS * (BLOCK / FRAME + 2)`，提供完整块及控制头的排队空间，使下一块读盘/校验能与 socket 发送重叠。过小的帧队列会在校验完成前耗空，造成间歇停顿。

- 每流维护至多两块的有序确认队列；窗口约束在途载荷，`Close` 等待所有确认。
- ACK 队列容量为窗口块数加一；主 socket 循环使用 `try_send` 转发，避免与数据队列相互等待。
- Linux/Android 队列保存文件区域描述；其他平台帧共享块缓冲，避免逐帧复制。
- 单一 socket 写入者完整写完一帧后才调度下一帧，防止头部与载荷交错。
- 两任务、每任务四流的载荷信用合计最多 64 MiB；这是信用上限，不是应用 RSS 上限。

### 平台发送后端

| 平台 | 当前发送路径 |
| --- | --- |
| Linux / Android | 先读块校验，再由 `sendfile` 发送快照区域；不支持时精确偏移回退 |
| Windows | 优先 `TransmitFile`，不可用时使用已核验的共享块缓冲 |

哈希工作在阻塞线程池执行，不占用异步 socket 执行器。已由接收端核验的恢复前缀不在发送端重复读取。接收端仍使用用户态分块校验，因此不是端到端零拷贝。

Linux/Android 内核路径处理短读写、`EINTR` 和 `EAGAIN`；不支持的系统调用错误允许回退，磁盘满等真实 I/O 错误终止任务。`sendfile` 回退从成功发送的准确位置继续。工作线程局部屏蔽并处理 SIGPIPE，不改变 Android 进程全局信号处理。取消可停止后续工作，不能强制中断已进入内核的磁盘/FUSE 调用。内核复制测试保留独立的 `splice` 接收辅助实现，仅在测试构建中编译，不提供协议入口。

## 接收端、检查点与发布

### 磁盘状态

状态位于配置数据目录的 `transfers-v2/`：

| 记录 | 内容 |
| --- | --- |
| `outgoing/<id>/task.json` | 清单、恢复凭据（64 位十六进制字符串）、地址、完成结果 |
| `outgoing/<id>/data/<index>` | 逐步增长或已完整拥有的任务快照 |
| `outgoing/<id>/data/<index>.capture.json` | 来源身份/区域、可选 URI、已持久化块摘要 |
| `incoming/<id>/receipt.json` | 清单摘要、凭据、持久偏移与哈希、初始目标、发布状态及结果 |
| `incoming/<id>/manifest.json` | 接收清单 |
| `incoming/<id>/direct-target.json` | SAF 直写对象映射、创建/发布意图与完成 URI |

完成后清理大体积快照与暂存，保留小型完成记录用于重复确认。暂停和断线保留任务。准备失败清理该次未登记快照；任务列表扫描在跨进程准备锁保护下清理崩溃遗留的未登记快照。

### 持久化与恢复

每个活跃接收流复用一个文件或 SAF FD，不缓存全部清单文件的句柄。写入仅持有对应文件锁；完成、取消或关闭前等待相关写入和同步结束。

每任务累计 32 MiB 或约 1 秒触发后台同步，文件结束、暂停和提交前强制同步：

1. 提交锁串行化检查点代际，短暂持有 pending 锁捕获偏移、哈希和脏 FD。
2. 释放 pending 锁后逐文件同步，再保存该代检查点。
3. 仅移除仍停留在捕获偏移的脏项；同步期间新增写入留给下一代。

JSON 经同目录临时文件同步并替换，Unix 同步父目录。同步失败保留脏状态；32 MiB 和 1 秒是触发条件，不是阻塞存储上的硬时限或最大重传量。

恢复时持有任务独占锁，逐块核验日志记录的前缀，截断未确认或损坏后缀，再返回偏移。锁保留至会话及在途阻塞写结束；旧会话未退出时重连返回 BUSY。流式哈希与持久偏移一起提交，恢复不能改变目标目录。

### 发布与重名

`layout` 为 File / Directory / Batch，兼容缺省 Legacy：单文件直接保存，目录保留根名，多文件使用独立任务目录。

- 本地先写 `.portalo-<id>.part`。发布锁下选择原名或 `.1`、`.2` 后缀，并持久化目标及发布意图。
- 单文件以同文件系统 hard-link 无覆盖发布；目录使用无覆盖 rename，Linux/Android 使用 `renameat2(RENAME_NOREPLACE)`。
- 发布前同步目录树，发布后持久化结果。单文件 link 后的崩溃缺口可用源/目标身份恢复；目录发布缺口严格核验路径集合、类型、空目录、大小与哈希。
- 没有发布意图时，不凭同名或同内容目录认领结果；已完成记录可处理最终确认丢失。
- 不生成或依赖 `.portalo-receipt`，不兼容旧标记目录，也不自动过滤或删除用户已有标记。

SAF 发布约束见下一节；提供方不保证文件系统级原子无覆盖重命名。

## Android SAF 适配

### 发送来源与所有权

`SendDocuments` 验证持久 URI 授权、普通可定位 FD、已知逻辑区域边界，并测试重新打开。Rust 保存 URI、offset/length 与来源身份，每批重新打开后验证；片段读取严格加上原始偏移。来源删除、移动、修改或授权失效会返回任务错误，不换用其他文件。

`SendSource::OpenFile` 的 FD 只能消费一次，避免克隆后共享偏移；Java 与 Rust 各自释放拥有的描述符。缓存使用唯一名称和引用计数清理，普通用户路径不受缓存删除语义影响。

无法持久重开的 FD、未知长度或管道来源先完整缓存；目录和多选由 `TransferDocuments` 导出，保留空目录并限制枚举深度、数量及循环。已完成的私有缓存优先硬链接到任务快照，不支持时回退至 reflink/复制；交接后 Java 不再写缓存。

Java 准备任务报告已缓存字节，支持取消及 30 秒无进展超时；最多两个工作线程、无排队。查询/打开使用可用的 `CancellationSignal`，管道使用非阻塞读取与短 poll。普通文件/FUSE 或提供方调用仍可能无法立即取消；UI 结束等待后，线程和临时文件需等调用返回才能回收。

### 接收后端

配置中的 `document_tree: {uri,label}` 与本地路径分开建模。旧配置继续使用 `download_directory`，`content://` 不被当作本地路径。

| 后端 | 条件与行为 |
| --- | --- |
| 直接写入 | `DirectTransferStorage` 对本机外部存储提供方实际探测普通 FD、随机读写、截断、同步及文件/目录重命名；成功后写目标临时文档，完成后发布 |
| 内部暂存兼容路径 | 其他提供方、探测失败或旧任务先接收至内部存储，提交时复制到提供方临时对象，再发布 |

任务建立后固定后端；直写开始后权限或能力变化会报错，不静默切换。Rust 经 JNI 按需取得独立 FD，不逐块传递 Java 字节数组。URI 映射共享引用，恢复时重新枚举，以适配 rename 引起的 URI 变化。

Java 使用 AtomicFile 保存创建意图、目标名称、临时对象、ready 状态和最终 URI。发布后日志未完成的缺口需核验结构和内容：直写由 Rust 核验 BLAKE3，兼容复制路径使用 SHA-256。已 ready 的兼容对象不再次复制整份文件；存在发布意图的取消先解析完成状态，避免删除成品。

兼容复制优先 `FileChannel.transferTo`（每次最多 8 MiB），不支持或返回零时以 256 KiB 缓冲从准确偏移继续。复制进度每 10 秒报告。按目录串行提交并预查重名；外部应用同时创建对象的竞态、rename 可见性和云端持久化仍取决于提供方。

## Windows 原生路径

`windows_io.rs` 在连接上查询 `TransmitFile`，使用单一 socket 写入者、活跃文件句柄缓存、64 位显式偏移和每帧不超过 256 KiB 的 overlapped 请求。独立 event 的低位抑制 IOCP 通知，避免 Mio 接收不属于它的完成结构。

阻塞任务持有 OVERLAPPED、event、socket 和文件直到终态。取消或超时请求 `CancelIoEx` 并排空完成；异常驱动阻塞时不能提前释放内核仍使用的对象。120 秒截止时间包含工作线程排队时间。

仅初始化不可用，或同步返回“不支持且请求未启动”时允许缓冲回退；同帧长度前缀不重复发送。IO_PENDING 后的失败、超时和短发送终止连接，通过任务恢复处理。

块克隆使用 `FSCTL_DUPLICATE_EXTENTS_TO_FILE`，先探测卷能力和簇大小，按 64 MiB 分段克隆对齐前缀，尾部有界复制。只处理从偏移零开始的完整普通文件；回退前截断部分克隆结果，真实 I/O 错误直接报告。通常要求同一 ReFS 卷，不假定 NTFS 支持，不修改或硬链接用户原文件。

## 发现与 UI

- mDNS 按服务实例跟踪设备，按主机名和端口聚合多地址；移除一个接口不删除其他地址，纯心跳不重建列表。
- 网卡枚举在 I/O 线程池，心跳复用缓存。Android 主机名由机型与持久应用随机后缀组成。
- 发送优先使用最新发现地址，记录过期时可尝试已选地址，不将 mDNS 缺失直接视为 TCP 离线。
- mDNS 初始化/浏览失败告警并每 5 秒重试，复用已创建 daemon；缺少发现管理器时安全跳过相关系统，不 panic。
- Bevy UI 前台约 100 ms、失焦约 1 秒定时唤醒，输入立即唤醒。进度使用有界 watch 快照，卡片复用实体，仅更新变化组件。
- 桌面选择器使用 `rfd::AsyncFileDialog`；触摸/点击由原始事件派发，避免低频更新丢失快速点击。

## 诊断与测试入口

### 阶段日志

| 日志 | 指标及用途 |
| --- | --- |
| `Transfer stage timings` | 接收 hash/write/sync/recovery/publication 累计耗时 |
| `Sender file stage timings` | 恢复偏移、总耗时、capture_wait/read_verify/network_queue/ack_wait；区分恢复等待与持续吞吐下降 |
| `Network transfer phase finished` | 包含流式捕获、校验与传输，不等于纯网络时间 |
| `DEBUG Payload copy` | 内核/回退字节数、调用次数、管道容量；PORTALO2 每帧输出，不宜在大文件测速时持续开启 |

阶段可重叠，累计耗时不是独立带宽。发送线程暂停/退出也记录统计，但取消时尚未结束的单次等待不计入分项。

```sh
adb logcat -s RustStdoutStderr
RUST_LOG=portalo_network=debug ./target/release/portalo
```

### 环境变量

| 变量 | 作用范围 |
| --- | --- |
| `PORTALO_DOWNLOAD_DIR` | 启动时覆盖接收目录 |
| `PORTALO_BUFFER_KIB` | 内核发送后端回退缓冲，64–1024 KiB，默认 256 |
| `PORTALO_DISABLE_ZERO_COPY` | 禁用 Linux/Android 内核复制快速路径，不控制 Java 复制或 Windows `TransmitFile` |
| `PORTALO_DISABLE_SENDFILE` | 禁用 Linux/Android `sendfile` 和 Windows `TransmitFile` |
| `PORTALO_DISABLE_REFLINK` | 禁用 Windows 块克隆 |
| `PORTALO_REFS_TEST_DIR` | Windows 克隆测试使用的 ReFS 目录，设置后要求克隆成功 |

禁用开关按变量是否存在判断。手动热缓存回环基准（不包含完整发布/确认流程）：

```sh
cargo test --release --locked --workspace --lib benchmark_kernel_and_buffered -- --ignored --nocapture
```

### Android instrumentation

`PreparationTest` 覆盖缓存边界与取消，`SendDocumentsTest` 覆盖 URI 重开及区域验证，`TransferDocumentsTest` 覆盖目录导出和可恢复发布，`SafStorageTest` 覆盖接收目录授权及验证探针清理。`SafStorage` 仅用于目录选择验证，不再提供传输会话或发布接口。

需要文档树的测试使用名称以 `Portalo-SAF-` 开头的专用空目录，先在应用中选定。应用与测试 APK 必须使用匹配签名。先按 README 构建 Rust 库，再执行：

```sh
cd android_app
bash gradlew assembleDebug assembleDebugAndroidTest
adb -s DEVICE install -r build/outputs/apk/debug/Portalo-debug.apk
adb -s DEVICE install -r build/outputs/apk/androidTest/debug/Portalo-debug-androidTest.apk
adb -s DEVICE shell am instrument -w -r \
  -e class org.bevyengine.portalo.SafStorageTest \
  org.bevyengine.portalo.test/androidx.test.runner.AndroidJUnitRunner
```

替换类名可运行其他套件。`SafStorageTest` 的 `-e revokeTestTree true` 会撤销测试目录授权，之后需重新选择。

## 验证范围与实测记录

以下为截至 **2026-10-24** 的记录。构建检查、自动化测试与端到端实测分别列出，历史版本结果不代表当前版本已完成同样规模的验证。

### 当前检查状态

| 项目 | 结果与边界 |
| --- | --- |
| Rust workspace lib 测试 | 移除旧协议后 62 项通过，1 项手动基准忽略；包含旧魔数拒绝且不创建输出的回归 |
| 严格 Clippy | Linux all-targets、Android/Windows workspace 通过 |
| Android Java 编译 | 移除旧协议后应用及 instrumentation 测试编译通过；目录验证不再依赖旧接收会话 |
| Release 构建 | 移除旧协议后 Linux x86_64、Windows x86_64 MSVC、Android arm64 Rust 库通过；Debug APK 打包与覆盖安装通过 |
| Android 真机 | Sony J9110 / Android 11；移除旧协议后 PreparationTest、SendDocumentsTest、TransferDocumentsTest 共 7 项通过；SafStorageTest 的专用目录套件本轮未运行 |
| Linux 启动与发现 | GUI 启动通过；注入 mDNS EPERM 后不崩溃，解除注入后恢复；Android 冷启动及后台返回验证通过 |
| Windows 原生运行 | 尚未验证 `TransmitFile` 和 ReFS 实机行为；交叉构建不能替代运行验证 |

Rust 回归覆盖并行目录/空文件/空目录、半帧断线、暂停重启、损坏检查点回退、源替换、文档非零区域、会话互斥、取消排空、完成确认丢失、发布缺口、清单兼容及有界预取。发送队列测试验证前一块未被 socket 消费时，下一块已进入校验。缓冲回退路径也完成了暂停续传回归。

移除旧协议后的 Release 验证：Linux GUI 启动和 5 轮 64 MiB 回环内容比对通过；Linux、Android 实际连接发送旧魔数均被立即关闭。Linux → Android SAF 直写 160 MiB 多文件、空文件和嵌套空目录完整 SHA-256/结构验证通过，准备到确认约 4.56 秒；另一个任务在 4 MiB 持久偏移暂停、重启手机应用后恢复并通过完整校验，发布结果记录缺口恢复返回同一目标。仅清理独立 UUID 测试输出，手机设置保持一致。

### 1 GiB Android → Linux 性能对比

同一已有文件、顺序测试；每组结果均通过完整 SHA-256。速率与时长采用各自表明的采样区间，不含选择器自动化等待，不作为普遍吞吐保证。

| 优化 | 调整前 | 调整后 | 口径 |
| --- | ---: | ---: | --- |
| 后台预取 | 35.086 秒 | 20.240 秒 | 首批接收到完成 |
| 后台预取 | 0.568 秒 | 0.324 秒 | 每 20 ms 采样的最长文件增长间隔 |
| 后台预取 | 2 次 | 0 次 | 超过 0.5 秒的增长间隔 |
| 完整块排队：暂停前 | 46.72 MiB/s | 45.88 MiB/s | 接收检查点增长采样 |
| 完整块排队：续传后 | 35.05 MiB/s | 47.15 MiB/s | 接收检查点增长采样 |

队列对比分别从 296 / 292 MiB 持久偏移恢复，剩余网络阶段耗时 19.862 / 15.162 秒。读取校验累计 6.367 / 6.913 秒，队列等待 12.873 / 0.001 秒；确认等待相应变化。这支持校验与网络写入重叠得到改善，不表示读盘成本消失。

### 大文件与恢复验证边界

| 阶段 / 方向 | 已验证内容 | 未完成或限制 |
| --- | --- | --- |
| 流式版本，Linux → Android | 160 MiB 四文件、空目录完整校验；4 MiB 暂停重启及发布恢复 | 准备到确认约 6.37 秒，不能外推大文件持续吞吐 |
| 流式版本，Android → Linux 8 GiB | 实际 URI 来源；约 128 MiB 强制结束发送端后可重启续传，持久接收到 3504 MiB | 随后暂停，未完成全文件 SHA-256 |
| 流式版本，Linux → Android 8 GiB | 清单准备约 0.009 秒；开始传输 | 约 2.4 GiB 时中断，设备显示 UID 网络限制；未完成完整校验，后续启动崩溃已另行修复 |
| 来源接口测试，Android 8 GiB | 独立稀疏文档两次重开、最后字节读取、错误区域与删除拒绝 | 这是来源接口测试，不是 8 GiB 端到端传输 |
| 历史完整预准备版本，Linux → Android | 160 MiB 直写目录完整校验，约 3.71 秒；另有全零 8 GiB 文件完整校验，约 693.5 秒 | 8 GiB 流程包含长时保存，不能作为当前流式/直写性能基线 |

历史 8 GiB 测试每 10 秒采样的应用 VmRSS 最大约 250 MiB，包含 UI/JVM/native，不是连续峰值或传输缓冲上限。早期回环基准为热缓存、不包含完整存储提交，不用于推断 Wi-Fi 性能。

仍待验证：Windows 原生与 ReFS、真实云端 SAF 提供方、当前流式版本双向 8 GiB 全流程。Android 目录和多选仍先导出，未知长度/不可重开来源仍先缓存。
