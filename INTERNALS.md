# Portalo 内部实现说明

面向开发者和贡献者。用户使用文档见 [README.md](README.md)。

## PORTALO2：目录、检查点与复用流

UI 使用 `crates/network/src/transfer/`。TCP 49527，每任务一个连接；接收端最多两个任务，每任务四条文件流（最多八条接收文件流）。所有多字节长度均为小端。

### 帧与状态

1. 发送 8 字节 `PORTALO2`，随后发送 `Hello {version:2}`；接收端取得任务槽位后返回 `Ready {streams:4,frame:262144,block:4194304,window:8388608,streaming:true}`，满载返回 `Error {code:"BUSY",...}`。window 缺省 4194304。streaming 缺省 false；新流式任务遇到不支持的接收端会提示更新，原完整快照任务仍可发送。
2. `Begin {manifest,token}` → `Resume {offsets,result}`。已有完成记录时直接返回 result；否则 offsets 是接收端重新核验后的各文件持久位置。
3. 每文件 `Open {file}` → 多个 `Data {file,offset}` → `Checkpoint {file,offset,credit,durable}` → `Close {file}` → `Closed {file}`。目录不占流。offset 表示已校验写入并返还信用的位置；durable 是该文件当时已提交的持久位置，缺省为 0 以读取旧消息；恢复始终使用接收端 Resume offsets。
4. 所有文件 Closed 后发送 `Commit`；接收端每 10 秒发送 `Saving` 存活通知，Android SAF 使用 `SaveProgress {current,total}` 报告实际复制字节数；发布并持久化结果后返回 `Complete {result}`。收到 Complete 才成功。Saving 不表示已写入字节数，也不证明提供方持续取得进展；阻塞的提供方调用目前仍不能强制中止。恢复前缀核验也发送 Saving，避免大文件重新校验超过网络超时。两端均需更新到支持这些消息的版本。
5. `Pause`/`Cancel` → `Stopped`；Pause 保留磁盘数据，Cancel 等待在途写入结束后删除未发布内容。已发布结果不可通过取消删除。断连/错误保留已确认检查点。进入 Commit 后提交运行至结束，连接 EOF/读取超时不再丢弃提交会话；保持任务锁，重复连接无法并发复制。发送端断线后结果仍持久化供再次恢复查询。

控制消息是 serde 外部标记枚举的 UTF-8 JSON，前缀为 u32 字节长度，范围 1..16 MiB；例如 `{"Hello":{"version":2}}`，无字段消息编码为 `"Commit"`。Data 消息后额外跟随 u32 载荷长度和原始字节，载荷范围 1..256 KiB。清单 JSON 限制为 8 MiB，封装后的消息仍须满足 16 MiB 上限。帧读写及控制等待超时为 120 秒；入口魔数等待 30 秒。连接按地址顺序尝试，每地址 2 秒，总共 10 秒。

新端每流最多两块（8 MiB）信用；最多两任务、每任务四流，合计载荷信用上限 64 MiB，不代表应用 RSS 上限。发送端维护至多两块的有序确认队列，可以在前一块确认前发送下一块；Close 必须等待全部块确认。ACK 队列为窗口块数加一，主 socket 循环使用 try_send，避免转发 ACK 时与数据队列相互等待。接收端帧队列有界，校验写入后返还 credit，尾块按实际长度返还。单流块内仍按顺序核验写入，网络可以并行接收下一块。

每任务累计 32 MiB 或约 1 秒触发后台同步，同任务最多一个后台同步任务；文件结束、暂停和提交前强制同步。提交锁串行化检查点代际：短暂持有 pending 锁捕获已写入 offsets 与脏 FD，释放后逐文件 fsync，再保存该代 offsets。仅移除仍停留在捕获偏移的脏项；同步过程中新增的写入留给下一代，不将它们提前标为持久。写入只持有自己的文件锁，其他文件可同时写入；同步只会短暂阻塞同一个文件。错误保留脏状态，恢复只认持久日志。1 秒是调度目标，32 MiB 是触发阈值，都不是阻塞存储上的硬时限或硬性重传上限。

每个活跃接收流仅打开一次文件/SAF FD，脏批次共享该句柄；流关闭前等待同步，取消前等待全部 worker 与同步任务、释放句柄后清理。不会缓存所有清单文件的 FD。Commit 不再重新打开并同步所有已完成文件，仅同步目录树与发布记录。并行数保持四流，其他阈值待实测调优。

SAF 文件 URI 映射以 Arc 共享且不写入每次检查点日志；恢复时由 Java 根据内部根记录和清单重新枚举，兼容 rename 导致的 URI 变化。避免每个检查点复制并重写整个目录的 URI 列表；旧日志中包含的 URI 字段仍可读取。

Linux/Android 发送流先读取并核验待发送块，再向有界队列提交文件区域描述；单一 socket 写入者按帧调用既有 sendfile 后端，复用其精确偏移回退、SIGPIPE 和取消逻辑。每帧完成后才调度下一帧，避免不同流头与载荷交错。发送哈希移到 blocking pool，不占用异步 socket 执行器。Windows 优先 TransmitFile；保留 Arc 共享已核验块及帧区间用于不支持时回退，不再逐帧 to_vec，最后一帧引用释放后尽量复用块缓冲。已由接收端核验的恢复前缀不在发送端重复读取。接收端仍使用用户态分块校验；不能将这一实现称为端到端零拷贝。逐次内核复制诊断降为 DEBUG，避免 8 GiB 任务产生数万条 INFO 日志。

### Windows 原生路径（PORTALO2）

`crates/network/src/windows_io.rs` 按连接通过 WSAIoctl 查询提供方的 TransmitFile 函数；复制 socket handle 供阻塞任务独立持有，按活跃文件缓存顺序读取句柄。单一写入者先发帧头与长度，再等待一帧（≤256 KiB）的 overlapped 原生发送完成，之后才发下一帧。Offset/OffsetHigh 使用清单中的 64 位显式偏移。使用独立 manual-reset event，并设置 OVERLAPPED.hEvent 低位抑制 IOCP 通知，避免 Mio 收到不属于它的完成结构。不设置 TF_WRITE_BEHIND、断连或 socket 复用标志。

原生请求的 OVERLAPPED、event、socket 和文件由阻塞工作线程持有到终态；等待 future 被丢弃时通过原子标记请求取消，工作线程最多每 50 ms 检查一次，调用 CancelIoEx 并排空完成。120 秒截止时间也包含工作线程排队时间；取消排空若被异常驱动阻塞，仍须保留对象，不能提前释放内核正在使用的内存。

仅初始化不可用或 TransmitFile 同步返回“不支持/参数不兼容且操作未启动”时切换缓冲路径。同帧长度前缀只发送一次，回退接着写该帧载荷。IO_PENDING 后的失败、取消、超时或短发送均终止连接并由任务恢复，不在字节数不明确时重发载荷。`PORTALO_DISABLE_SENDFILE=1` 可禁用 Windows 原生发送；每连接日志记录完成的原生载荷字节数。

写时复制使用 `FSCTL_DUPLICATE_EXTENTS_TO_FILE`：先查询源和目标卷的 `FILE_SUPPORTS_BLOCK_REFCOUNTING`，再查询实际簇大小。仅处理从偏移 0 开始的完整普通文件；目标先扩展到逻辑长度，按 64 MiB（小于 API 的 4 GiB 限制）分段克隆簇对齐前缀，尾部不足一簇用有界复制。分段间检查准备取消。跨卷、不支持、稀疏/完整性属性不兼容等错误回退前先截断全部部分克隆结果；磁盘满等真实 I/O 错误直接报告。原用户文件不会被硬链接或修改。通常需要同一 ReFS 卷，NTFS 不假定支持。`PORTALO_DISABLE_REFLINK=1` 可禁用 Windows 块克隆。该分段过程不是整个活动源文件的原子时间点快照，仍沿用准备期源修改检查与不可变任务快照语义。

### 清单与磁盘状态

Android 单文件来源现可将持久授权的 content URI 交给流式准备。选择时验证普通 FD/已知长度/逻辑偏移，并测试重开；Rust 再比对新旧 FD 的大小、mtime、dev/inode，不读取完整载荷。capture.json 保存 URI、逻辑 offset/length 及源身份；捕获批次重新打开 URI，批次前后验证源身份，对片段严格加上原始偏移。首块 4 MiB 立即持久化，后续每批最多 32 MiB，复用源句柄并合并快照同步/日志保存；仅用额外一个 4 MiB 临时缓冲，不将整批驻留内存。该批持久化完成后才允许发送，网络仍是 4 MiB 校验块与每流 8 MiB 窗口。捕获完成后移除来源依赖。JNI 异常转为可恢复任务错误，源删除/授权撤销不会回退到其他文件。无法取得持久授权的 FD 保留原完整准备路径；目录和多选导出尚未改为按文档流式扫描。

为避免批次边界串行等待，独立后台任务提前捕获下一批：未消费的持久前缀达到 32 MiB 时等待，单批最多再追加 32 MiB；这限制新增磁盘预取量，不在内存保留整批。发送端读取原子发布的日志和已持久化块，不获取下一批写入锁，并验证块哈希。取消时同时终止后台任务并等待不可中断的捕获工作结束，再清理快照。新增回归覆盖持锁期间已提交块可读、捕获超前于网络窗口及退出排空；当前 Rust 单元测试 71 项通过，1 项手动基准忽略。

2026-10-24 Sony J9110 Android → Linux 同一已有 1 GiB 文件对比：串行批量版本与后台预取版本分别从首批接收至完成耗时 35.086 秒、20.240 秒。每 20 ms 采样接收文件增长，最长增长间隔分别 0.568 秒、0.324 秒，超过 0.5 秒的间隔分别 2 次、0 次。两次接收与手机原文件 SHA-256 均为 `49bc20df15e412a64472421e13fe86ff1c5165e18b2afccf160d4dc19fe68a14`。这是一轮同设备顺序测试，不是长期吞吐保证；选择器自动化等待不计入对比区间，也不作为准备耗时。报告和原始采样在 `/tmp/opencode/portalo-1g-ab/{before,after}.json`。三端严格 Clippy 和 Release 构建通过；新 APK 已覆盖安装，设置及原文件保留。

- 任务 ID 为 UUID，文件 ID 为清单索引；路径为 UTF-8 分量数组。父目录先出现，禁止重复/大小写冲突、不一致父目录大小写、特殊名称和 `.portalo-` 保留前缀。最多 100000 项、64 层，每分量 240 字节。
- 清单摘要是 serde 序列化结果的 BLAKE3。流式任务 `streaming:true` 的清单只固定路径与大小，blocks 为空；每块 Data 前先发送 `Hash {file,offset,hash}`。接收端按流严格检查顺序、校验内容，再把分块摘要和持久 offsets 一起原子保存。恢复与发布缺口核验使用持久摘要。原完整快照任务仍在清单中保存摘要；streaming 为 false 时省略，旧摘要不变。空文件无分块，空目录无载荷。
- `transfers-v2/outgoing/<id>/task.json` 保存清单、随机生成的 64 位十六进制发送方恢复凭据、地址和结果；`data/<index>` 保存逐块增长的快照，`<index>.capture.json` 保存源路径、大小、修改时间、文件身份和已持久化的块摘要。每块先同步快照、原子保存摘要再入发送队列；两块窗口重叠下一块准备与上一块发送。暂停/取消等待在途阻塞捕获结束。reflink/私有缓存已拥有整份内容时只按需读哈希。普通复制来源每块读取前后检查元数据，尚未暂存内容依赖原文件，不能在完成前移动/修改；元数据检查不提供对蓄意还原时间戳等并发修改的强一致性保证。整个目录也不提供瞬时快照。
- `incoming/<id>/receipt.json` 保存清单摘要、凭据、持久 offsets、初始目标和完成结果。恢复不能改变目标目录。fs2 独占任务锁从核验持有至会话和在途阻塞写结束，相当于单一写入代际；旧会话未释放时新会话返回 BUSY 错误。
- 接收数据先写 `.portalo-<id>.part` 任务目录。恢复逐块核验已经记录的前缀，截断未确认/损坏后缀，再返回 offsets。重连不能靠发送方声明跳过数据。
- 文件同步早于检查点；JSON 通过同目录临时文件同步并替换，Unix 同步父目录。发布前同步目录树并记录 publishing 意图；Linux/Android 本地目录用 `renameat2(RENAME_NOREPLACE)` 发布，再同步父目录和结果记录。不生成 `.portalo-receipt`；rename 后结果记录前的异常恢复核验完整路径集合、类型、空目录、大小与每块 BLAKE3。没有发布意图时不根据目录内标记认领目标；目录核验不豁免旧标记条目。内部另存 manifest.json。
- 完成后删除发送快照及 SAF 本地暂存，保留小型完成记录用于重复确认；暂停/断线不会自动删除任务。任务准备失败会删除该次未完成快照；任务列表加载时，在跨进程准备锁保护下清理崩溃遗留的未登记快照。

### 新任务保存布局

清单新增 `layout`：File / Directory / Batch，缺省 Legacy。Legacy 序列化时省略字段，确保已保存清单摘要不变。File 必须只有一个同名文件；新建文件夹任务保留选择的根目录名，多文件使用独立批量任务名。

本地发布在内部全局发布锁下选择原名或 `.1` / `.2` 后缀，并先持久化目标及 publishing 意图。目录用不覆盖 rename 发布；单文件用同文件系统 hard-link 原子发布、写完成记录后删除暂存。崩溃在 link 和记录之间时，以源与目标的文件身份恢复结果。目录仅在临时源消失且存在发布意图时执行完整内容核验；普通同名/同内容目录不用于去重。原任务凭据、清单摘要和最终位置不变。

SAF 兼容路径的发布日志先记录目标名称，临时对象使用任务 UUID 名称。单文件创建临时文件而非目录，ready 后直接 rename；恢复 rename 后日志未完成的单文件时按 SHA-256 对比暂存内容，再持久化最终 URI。有同名对象且临时对象仍存在时重新编号。目录恢复比较完整结构和 SHA-256，不依赖目标标记，也不忽略目标中的旧标记。已移除 markerless/legacyMarker 分支；本地遍历与 SAF 导出也不再过滤旧 `.portalo-receipt`，保留前缀仍由清单准备阶段拒绝。SAF 没有通用的原子不覆盖 rename；应用按目录串行提交并预查重名，外部进程并发创建的竞态仍受提供方实现限制。

### Android SAF 目录

`TransferDocuments` 将所选文档树/多文件导出为应用缓存。已完成导出的 Cached/FolderCache 按不可变私有数据交接给 Rust，优先 hard-link 到 outgoing/data/<index> 后计算 BLAKE3；原缓存最后一个 owner 清理时只删除旧名称，任务持有的数据保持可用。硬链接不可用则走 reflink/复制。仅用于应用拥有的缓存，普通 Path/OpenFile 来源仍独立建立快照；Java 在交接后不得再写缓存。准备失败清理新任务目录，未完成任务不依赖仍存活的 FD。未知长度来源在准备阶段读完。空目录保留，枚举有深度/数量和循环限制。

新任务通过 `DirectTransferStorage` 对 `com.android.externalstorage.documents` 做一次实际能力探测（普通 FD、随机读写、截断、fsync、文件及目录 rename）；成功后直接写所选树中的临时文档。Rust 按需通过 JNI 获取拥有所有权的 FD，沿用 BLAKE3 与批量持久检查点。`incoming/<id>/direct-target.json` 用 AtomicFile 记录后端、创建意图、临时名称、根 URI、目标和完成 URI；rename 后重新枚举获取新 URI，结构检查通过后由 Rust 核验完整 BLAKE3。结果明确后才记录完成。发布意图存在的取消先要求恢复解析结果，避免误删成品。

其他提供方、能力探测未通过或旧任务使用内部暂存兼容路径，不要求随机写；提交时复制到提供方临时目录，AtomicFile 记录创建意图、URI、ready 和最终 URI。提供方关闭流不等于远端云同步，目录 rename 的可见性取决于提供方。不在已经开始直写后静默换后端；权限撤销、空间不足或 FD 能力变化均明确失败。

SAF 复制优先 `FileChannel.transferTo`，每次最多 8 MiB；返回零或不支持时用 256 KiB 缓冲从当前准确偏移继续。已完成复制且日志 ready 的对象直接进入发布，不再次复制整个文件。Java 原子计数通过 JNI 每 10 秒同步到发送端和接收卡片的保存进度。读写失败会明确报错，不能把磁盘已满等错误视作成功或无限重试。

### 验证范围

本轮构建验证：
- Android URI 来源及批量捕获改造后 `cargo test --locked --workspace --lib`：69 项通过，1 项手动性能基准忽略。覆盖 8 GiB 清单无需预读、源替换拒绝、已捕获块脱离源复用、流式并行目录、暂停续传、哈希检查点损坏回退与发布恢复、非零文档片段偏移边界及有界捕获批次的持久恢复。最新 Linux/Android 严格 Clippy 通过；Windows URI 改造版本通过严格 Clippy，批量捕获版本交叉 Release 构建通过。禁用 sendfile 的流式暂停续传也通过。
- Android → Linux 8 GiB 实际发送：通过系统选择器选择已有文件，任务持久记录确认 streaming 与文档 URI，约 128 MiB 时强制结束手机应用；重启后从任务列表继续，接收持久进度达到 3674210304 字节（3504 MiB）。用户随后暂停并关闭接收窗口，未完成全文件 SHA-256。批量捕获版按用户要求仅更新安装，尚未进行同文件速度对比，不宣称提速幅度。原文件、双方任务和 Linux 隔离测试目录 `/tmp/opencode/portalo-android-send-validation/config` 均保留。
- `SendDocumentsTest` 在 Sony J9110 上通过：新建独立稀疏 8 GiB 文档，两次重开并定位读取最后一个字节；错误区域和删除后的来源均拒绝，测试文件已清理。
- Linux/Windows `--all-targets`、Android workspace 的严格 Clippy（`-D warnings`）均通过。
- Linux x86_64、Windows x86_64 MSVC Release 构建和 Android arm64 Release Rust 库、Debug APK 打包通过。Android APK 使用与现有安装匹配的开发签名，签名校验及覆盖安装通过。
- Sony J9110 / Android 11：`TransferDocumentsTest` 与 `PreparationTest` 共 6 项通过，包含 SAF 直写文档重开、rename 后恢复、兼容暂存发布、同名编号、空目录与准备取消。
- 禁用 sendfile 后，目录多流暂停/重启/完成确认回归用例通过。
- Linux Release 图形启动冒烟通过：Vulkan 初始化并创建窗口，运行 10 秒后由测试超时主动结束；`ldd` 无缺失库。
- 流式准备改造后的真机验证：160 MiB 四文件及空目录完整 SHA-256 通过，准备到确认约 6.37 秒；4 MiB 暂停后重启续传、发布记录丢失恢复均通过。该次短任务比下述完整预准备版本慢，流式改造的目标是缩短首发等待，并不保证提高总吞吐；逐块发送前的持久化有额外成本。
- 8 GiB 流式测试：使用每 64 MiB 含 4 KiB 随机内容的稀疏测试文件，清单准备约 0.009 秒后开始发送。传到约 2.4 GiB 时网络被中断；后续 Android netpolicy 显示应用 UID 为 `REJECT_ALL`，重启 mDNS 的本地信令也返回 EPERM，触发已有 `mDNS browse` expect。该项完整传输/SHA-256 未通过验证，不能据此报告 8 GiB 总耗时或完整正确性。需解除设备网络限制后重测。
- 后续启动闪退修复：mDNS 创建/浏览失败改为告警并每 5 秒重试，浏览失败时复用 daemon，避免重复创建后台线程。发现管理器缺失时相关系统安全跳过，网络异常不再触发初始化 panic。Android 旧进程经历 panic 后可能连续报 `RecreationAttempt`；覆盖安装并强制结束旧进程后再启动。发现模块新增缺少管理器时正常更新/退出的回归测试。
- 闪退修复验证：发现模块 2 项测试通过，Linux/Android 严格 Clippy 通过。Linux Release 使用临时 LD_PRELOAD 对 loopback sendto 注入 EPERM，启动与多次重试未崩溃，解除注入后重试告警停止。Sony J9110 修复 APK 连续 3 次冷启动及每次后台/前台切换通过，主页及附近设备正常显示，原设置保留。
- 流式准备改造之前，Linux Release → Sony J9110 SAF 直写端到端通过：四个数据文件合计 160 MiB，另含空文件和嵌套空目录；检查完成记录确认 direct 后端，SHA-256 与目录结构一致，无 `.portalo-receipt`。准备到最终确认约 3.71 秒，连同哈希/目录复核约 4.42 秒；这是一次短任务结果，不能外推为 8 GiB 持续吞吐。
- 真机直写单文件在 4 MiB 持久检查点暂停、强制结束应用后重启，从 4 MiB 恢复并通过完整 SHA-256；清除该测试任务的 Rust 完成结果后再次重启，恢复返回同一目标 URI，内容一致。仅清理本次 UUID 测试数据和记录，用户接收目录内容、主题及 SAF 授权设置保留。
- Windows 原生运行与 ReFS 实机测试尚未进行，不能将交叉构建视为原生路径性能或运行正确性验证。

原完整快照路径的阶段日志包括准备总耗时、每文件快照方式；网络阶段与接收端 hash/write/sync/recovery/publication 也记录耗时。流式任务的网络阶段包含按块暂存和哈希，不能把该时长当作纯网络传输时间。哈希仍覆盖全部内容，仅将准备工作与发送重叠。Android 无法重新打开的 FD/未知长度来源仍走完整准备路径，SAF 导出本身仍可能需要等待。

发送文件工作线程退出（包括暂停）时输出 `Sender file stage timings`，记录恢复偏移、总时间、等待捕获、流式读取/校验、等待网络队列和等待块确认的累计时间，用于区分续传前校验等待与续传后的吞吐下降。取消时尚未完成的单次等待不会计入分项；各线程阶段可重叠，分项不能作为独立带宽指标。

发送帧队列按 `STREAMS * (BLOCK / FRAME + 2)` 设置容量，允许每流至少一个完整已验证块排队，以便下一块读盘/校验与 socket 写入重叠；原来仅 `STREAMS` 个帧的队列总共约 1 MiB，可能在下一块校验结束前耗空。每流 8 MiB 确认窗口继续约束在途载荷，Linux/Android 队列只存区域长度，其他平台帧共享块缓冲。回归测试验证 socket 尚未消费第一块时，第二块已开始校验；72 项 Rust 单元测试通过，1 项手动基准忽略，三端严格 Clippy 通过。

同日 1 GiB 实机暂停续传对比：队列调整前暂停前/续传采样吞吐为 46.72/35.05 MiB/s，调整后为 45.88/47.15 MiB/s。分别从 296/292 MiB 持久偏移恢复，剩余网络阶段耗时 19.862/15.162 秒，两个最终文件 SHA-256 均与手机原文件一致。调整前续传捕获等待 118 ms、读取校验 6367 ms、队列等待 12873 ms；调整后为 1222/6913/1 ms，确认等待为 6966 ms，说明完整块排队使校验与 socket 发送重叠。这是一轮顺序对比，不能推断所有网络均有相同提升。报告 `/tmp/opencode/portalo-1g-ab/resume.json`、`resume-queue.json` 及 `sender-stage-log.txt` 保留；首轮自动化因无接收设备未进入传输而超时，不计入结果。

Windows 新增回归用例：稀疏文件上超过 4 GiB 的显式偏移、原生载荷与普通帧字节的顺序、不支持的原生请求回退时不重复长度前缀、过期截止时间不发送载荷、克隆尾部与源/目标写入隔离。默认临时卷可走复制回退；设置 `PORTALO_REFS_TEST_DIR` 为 ReFS 上的测试目录后要求块克隆成功。这些用例已通过 Windows all-targets Clippy 类型检查，尚未在 Windows 执行。

Rust 自动化覆盖并行目录/空目录/空文件、暂停重连、断连半帧、检查点损坏回退、清单冲突、源文件修改、会话互斥、取消清理及发布记录丢失后的去重；还覆盖原清单摘要兼容、单文件 hard-link 发布崩溃窗口、原名目录碰撞和超过 120 秒的提交存活通知。手动性能基准默认忽略。

以下为本轮存储优化之前的 Sony J9110 / Android 11 PORTALO2 真机基线记录，不代表本轮的性能数据：
- `TransferDocumentsTest` 的目录发布、rename 后完成记录丢失恢复、文档树导出、空目录保留，以及单文件直接保存、同名编号和 SHA-256 恢复通过；连同 `PreparationTest` 共 5 项 instrumentation 测试通过。
- Linux → Android SAF：一条流停滞时另一文件仍能确认；4 MiB 检查点后强制重启应用，恢复核验前缀并丢弃未确认分块；交错文件流、空文件和空目录全部通过 SHA-256/目录检查。
- 发布完成但不读取最终 ACK，重启应用后再次 Begin 返回同一结果，无重复发布；显式取消删除未完成暂存。
- Android 系统目录选择器 → Linux：16 MiB 目录及嵌套空目录通过；144 MiB 任务在 UI 暂停后强制重启发送端，未完成列表仍存在，点击继续后文件哈希全部正确，完成快照已清理。
- 新布局 Linux → Android：目录按原名保存，第二次同名传输自动 `.1`，嵌套空目录及文件 SHA-256 一致。新协议 sendfile 路径的并行/暂停集成测试通过；设置 `PORTALO_DISABLE_SENDFILE=1` 的缓冲回退同样通过。
- 新布局 8 GiB 单文件 Linux → Sony J9110 SAF 真机通过：直接发布到所选目录，最终确认及完整 SHA-256 一致（全零测试数据）。准备、网络、保存合计约 693.5 秒；保存通知持续超过 4 分钟，没有 120 秒误超时。每 10 秒采样的应用 VmRSS 最大值 256036 KiB（约 250 MiB，包含 UI/JVM/native，不是传输缓冲区单独占用，也不是连续采样峰值）。生成的 8 GiB 测试文件、暂存及手机端记录已清理，用户设置保持不变。
- 测试保持用户设置，测试文件使用独立目录。Windows 尚未进行本机运行测试，云端 SAF 提供方尚未进行实测。

## PORTALO1 兼容入口与零拷贝后端

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
- 连接按优先顺序逐个尝试，地址去重；每地址最多 2 秒，总截止时间 10 秒。PORTALO1 没有接收就绪握手，因此不对同一设备并行建立多个连接，避免争抢接收槽位。协议头、每次 I/O 和 ACK 等待最多 30 秒。

UI 发送与发现已切换到版本 2；以下缓冲区、零拷贝和单文件 SAF 章节描述保留的版本 1 后端。

## 缓冲区与环境变量

| 变量 | 含义 | 默认 |
| --- | --- | --- |
| `PORTALO_BUFFER_KIB` | 回退缓冲区大小，64–1024 KiB | 256 |
| `PORTALO_DOWNLOAD_DIR` | 覆盖接收目录（启动时读取） | — |
| `PORTALO_DISABLE_ZERO_COPY` | 强制使用缓冲路径（不控制 Android Java 端） | — |
| `PORTALO_DISABLE_SENDFILE` | 仅禁用发送快速路径 | — |
| `PORTALO_DISABLE_SPLICE` | 仅禁用接收快速路径 | — |

`PORTALO_BUFFER_KIB` 同时作为尽力申请的管道容量，内核拒绝增大时保留系统默认值。

## Linux/Android 零拷贝传输

"零拷贝"指有效载荷不经过应用用户态的中转缓冲区，不保证内核、驱动或文件系统内部没有复制。

| 平台 / 来源 | 发送 | 接收 |
| --- | --- | --- |
| Linux 普通文件 | `sendfile` | socket → pipe → 文件（`splice`） |
| Android 普通 SAF 文档（已知有效片段边界） | 文档 FD → `sendfile`，无缓存副本 | 对普通文件 FD 使用 `splice` |
| Android 未知长度、管道或不可直读文档 | 后台缓存到唯一临时文件后发送 | 非普通文件 FD 保留带超时的 Tokio 流式写入 |
| Windows | 有界缓冲发送 | 有界缓冲接收 |

此表为保留的 PORTALO1 后端；Windows `TransmitFile` 已接入 PORTALO2，PORTALO1 仍使用缓冲路径。

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
- 起始偏移非负且可定位
- 长度已知，起始偏移加长度不溢出且不大于底层文件大小

`SendSource::OpenFile` 只能被消费一次，避免克隆导致并发共享文件偏移。正常结束、失败、未采用或被新选择替换时均释放所有权。

缓存回退使用随机临时文件名，原始显示名称单独保存，避免相同名称的选择互相覆盖。缓存由引用计数清理对象管理，最后一个所有者释放后自动删除；普通用户文件路径不使用该删除语义。

Java 准备任务显示缓存字节数，支持取消和 30 秒无进展超时。每个任务独立保存终态，取消后的迟到结果不会发送到 Rust。可取消的查询/打开操作使用 `CancellationSignal`；管道/socket FD 使用非阻塞读取和最多 100ms 的 poll 等待，避免空闲管道无限阻塞取消。最多两个准备工作线程、无排队。普通文件/FUSE 调用及提供方打开操作仍可能无法立即取消，此时 UI 结束等待，但线程和临时文件要等调用返回后才能回收。

### 诊断

Linux/Android 内核复制工作任务结束时输出一次 `DEBUG Payload copy`（可设 `RUST_LOG=portalo_network=debug`），包含 `direction`、`bytes`（实际传输量）、`kernel_bytes`（经快速路径传输量）、`kernel_calls`（快速路径调用次数，含重试）和 `pipe_capacity`（接收管道实际容量）。PORTALO2 每帧一次，不建议大文件验证时持续开启详细日志。

```sh
# 强制缓冲路径
PORTALO_DISABLE_ZERO_COPY=1 ./target/release/portalo

# 手动回环基准（热缓存，不含 fsync，仅对比本实现的两条路径）
cargo test --release --locked --workspace --lib benchmark_kernel_and_buffered -- --ignored --nocapture
```

本机回环参考结果（64 MiB × 3 轮，调试版二进制）：
- 缓冲路径：2247–2324 MiB/s，CPU 45.9–47.4 ms
- 内核路径：1904–2039 MiB/s，CPU 42.9–45.6 ms

回环吞吐在内核路径下略低，CPU 小幅减少。Android 的直接收益是消除完整发送缓存的额外磁盘写入，这在回环基准中无法体现。

2026-10-23 release 四组合测试（64 MiB，每组合三轮，热缓存；未包含文件发布和 ACK）：

| 发送 / 接收 | 吞吐范围 MiB/s | 进程 CPU ms |
| --- | ---: | ---: |
| 缓冲 / 缓冲 | 1663–2419 | 44.1–59.9 |
| sendfile / 缓冲 | 2021–2070 | 41.8–43.7 |
| 缓冲 / splice | 2278–2482 | 43.3–47.2 |
| sendfile / splice | 2037–2089 | 42.1–43.5 |

接收管道为 262144 字节，splice 调用约 517–555 次/文件。本轮结果不支持把之前的回环吞吐下降直接归因于 splice，也不构成修改三端默认策略的依据。Windows 双缓冲需目标平台测量后再决定；实际 Wi-Fi 持续吞吐和冷盘性能尚未测量。

2026-10-23 修复后在 Sony J9110 / Android 11 验证：
- `PreparationTest` 三项通过：空闲管道连续三次取消均释放读取 FD 并允许下一任务；已知长度管道不等待 EOF、不读取逻辑边界外字节；关闭后的 SAF 实例在解析 provider URI 前拒绝操作。
- Wi-Fi TCP → 已授权 SAF 目录：两个同名 8 MiB 文件并发接收，其中一路暂停 2 秒，两路均 ACK 成功且 SHA-256 正确。日志确认各自 8388608 字节全部经 splice 写入，实际管道容量 262144 字节。
- 空文件保存并 ACK；声明 1 MiB、只发送 64 KiB 后断开，未发布最终文件，UI 正确显示失败。
- 测试生成的最终文件已删除，主题和目录设置逐字节核对未变。未执行需要专用测试目录的 `SafStorageTest` 套件；真实云盘提供方、准备取消按钮及非零偏移文档选择仍需端到端验证。

## Android SAF 接收存储实现

- 配置新增可选 `document_tree: { uri, label }`，旧配置无此字段时继续使用 `download_directory`。
- URI 与文件路径分开建模，不能把 `content://` 当成本地路径。
- Java 使用 `DocumentsContract` 创建唯一 `.portalo-<UUID>.part` 文档，通过 `ParcelFileDescriptor` 给 Rust 独立的文件描述符。Rust 直接流式写入，不先缓存整份文件，也不通过 JNI 逐块传递字节数组。
- 精确接收字节数、flush、关闭描述符后，查询已有文件名并重命名；同一 tree URI 的提交串行化，同名文件追加 `.1`、`.2` 后缀。成功后才写回 ACK。
- 会话表使用并发表，每会话锁保护描述符与终态；慢速准备、关闭和删除不持有全局表锁。准备前与登记前两次检查关闭状态。Activity 销毁立即设置关闭标记，再把各会话清理交给进程级、最多两线程的独立清理池，避免单个会话阻塞其余清理。清理池全阻塞时拒绝新增清理并记录错误，不能保证异常提供方下的即时删除。
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
- SAF：查询后重命名，串行化在 Java 每目录提交锁中完成。
- 同名文件追加 `.1`、`.2` 等后缀，不覆盖。

## 设备发现

- mDNS，设备按服务实例跟踪，按主机名及端口聚合多宿主地址。
- 纯心跳不触发设备列表重建；删除一个接口地址保留其他接口。
- 网卡枚举在 I/O 线程池，心跳使用缓存结果。
- Android 主机名使用机型加持久化应用随机后缀，不读取硬件标识。
- 发送优先读取最新发现地址；mDNS 记录缺失时保留已选设备地址尝试连接，不把发现缓存过期当作 TCP 离线。

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
