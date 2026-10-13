# Android 系统接收文件夹（SAF）

## 使用

1. 打开 Portalo → **设置 → 选择接收文件夹**。
2. 在系统选择器中进入目标目录，例如 `Download/Portalo`，也可以先创建子文件夹。
3. 点击系统的“使用此文件夹”，并允许读写访问。
4. Portalo 检查目录写入、临时文档重命名和删除能力；保存成功后，设置页显示“系统文件夹”、名称和 `content://` URI。
5. 新建立的接收任务写入该目录；正在进行的任务保留原目标。

授权会持久化，应用重启后仍有效，不需要“所有文件访问权限”。Android 11 及以上通常禁止选择存储根目录、Download 根目录、Android/data 和 Android/obb 等受限位置，需选择系统允许的子目录。

取消选择保持原配置。“恢复默认接收目录”切回应用内部目录；“使用应用外部存储目录”仍然提供原有的应用专属目录选项。
授权撤销、目录删除或存储介质移除后，接收会明确失败并在应用底部提示；不会悄悄改存到内部目录。

## 存储实现

- 配置新增可选 `document_tree: { uri, label }`，旧配置默认没有此字段，继续使用原 `download_directory`。
- URI 与文件路径分开建模，不能把 `content://` 当成本地路径。`PORTALO_DOWNLOAD_DIR` 在启动时显式切回路径模式。
- Java 使用 `DocumentsContract` 创建唯一 `.portalo-<UUID>.part` 文档，通过 `ParcelFileDescriptor` 提供一个独立的文件描述符给 Rust。
- Rust 以现有复用缓冲区直接流式写入，不先缓存整份文件，也不通过 JNI 逐块传递字节数组。
- 精确接收字节数、flush、关闭描述符后，才查询现有文件名并重命名。应用内提交操作串行化，同名文件使用 `.1`、`.2` 等后缀。成功完成这些步骤后才给发送方 ACK。
- 断连和 I/O 失败会关闭描述符并删除临时文档；删除失败会明确报错。任务取消有清理 guard，Activity 正常销毁也会清理打开的文档。
- 目录/provider 操作在工作线程执行；只在主线程打开系统选择器。Java 异常转换为 Rust I/O 错误并清除 JNI 的 pending exception，避免影响后续任务。
- 原目录授权暂不立即释放，以免切换目录时破坏已有接收任务。

## 支持边界

SAF 提供方必须允许创建、写入、重命名和删除文档。不能满足这些能力的提供方会在选择验证或接收时被明确拒绝。
重命名的可见性、跨应用竞争和远端持久化语义由 provider 决定，不承诺文件系统级原子发布或云端落盘。
系统强杀进程、设备断电或提供方失联可能遗留 `.part` 文件；目前没有崩溃恢复日志或断点续传。

已验证 Android 11 / Sony J9110 的系统外部存储 provider、Download 中的子目录。未实测 SD 卡、云盘 provider 和其他机型。

安装后恢复应用时曾遇到一次 Bevy/Winit 的 `RecreationAttempt`（同一进程内再次创建事件循环），结束应用进程后冷启动恢复；本轮正常选择器往返及强制结束后的冷启动测试通过。该 Activity 重建兼容性问题仍需单独排查。

## 验证结果（2026-10-13）

- 通过系统选择器选择 `Download/Portalo-SAF-20261013`，配置保存成功。
- 空文件、4 KiB、8 MiB、重复同名 8 MiB 接收并 ACK 成功，SHA-256 全部一致。
- 两次 8 MiB 单次计时分别约 0.461s、0.392s（17.3、20.4 MiB/s），含创建文档和提交确认，不作为持续吞吐基准。
- 强制结束应用进程后重新打开，无需再次授权即可接收。
- 两个并发同名传输保留不同内容并通过哈希校验。
- 中途断开 TCP，目录列表恢复到开始前的状态，没有最终文件或临时文档残留。
- 真实撤销测试目录的持久授权后，接收无 ACK、无文件、无内部目录回退，UI 提示重新选择。
- 取消目录选择保持配置；恢复默认目录后本地路径接收与哈希校验正常。
- Rust 回归共 19 项通过；Android 原生 instrumentation 测试 3 项通过。

### 运行原生 provider 测试

先在应用中选择一个名称以 `Portalo-SAF-` 开头的**专用空测试目录**。测试不操作普通用户接收目录。

```sh
# 在 android_app 目录构建；应用 APK 需先包含最新 Rust 库
bash gradlew assembleDebug assembleDebugAndroidTest
adb -s DEVICE install -r build/outputs/apk/debug/Portalo-debug.apk
adb -s DEVICE install -r build/outputs/apk/androidTest/debug/Portalo-debug-androidTest.apk
adb -s DEVICE shell am instrument -w -r \
  -e class org.bevyengine.portalo.SafStorageTest \
  org.bevyengine.portalo.test/androidx.test.runner.AndroidJUnitRunner
```

默认运行提交/同名/空文件、失败清理两项测试；授权撤销测试默认跳过。显式加入 `-e revokeTestTree true` 才会撤销该测试目录授权，之后需重新选择目录或恢复默认目录。
