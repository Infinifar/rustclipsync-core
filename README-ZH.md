# rustclipsync-core

[English](README.md) · 简体中文

[RustClipSync Desktop](https://github.com/Infinifar/rustclipsync-desktop)（Windows/Linux）与
[Omaclipsync 插件](https://github.com/Infinifar/omaclipsync)使用的共享 Rust 同步引擎。
提供协议 3 方向控制、持久化同步、加密状态、敏感收件箱和可选冲突恢复；
客户端分别提供剪贴板、存储与凭据的平台适配层。

## 0.4.0 正式发布

[发布说明](releases/v0.4.0.md) · [GitHub Release](https://github.com/Infinifar/rustclipsync-core/releases/tag/v0.4.0)

| 组件 | 配套版本 |
| --- | --- |
| 服务端 | 3.0.0，协议 3 |
| 共享 core | 0.4.0 |
| Windows/Linux 桌面端 | 0.5.0 |
| Omaclipsync | 0.4.0 |

移动端与 Fcitx 输入法单独发布；切换共享服务端前请确认所用构建支持协议 3，否则继续使用服务端 2.0.0。Android 当前通过 Kotlin 独立实现协议。

## 功能总览

| 能力 | 行为 |
| --- | --- |
| [设备方向与暂停](#设备方向与协议-3) | 仅推送、仅接收、双向同步；暂停保留已选方向。数据访问前先上报并等待 ACK，由服务端过滤分发，core 同时保留本地收发限制。 |
| [WebSocket 控制与 HTTP 降级](#设备方向与协议-3) | 复用认证后的 WSS 控制连接；失败时通过 HTTPS 重试同一操作。会话 ID 与递增版本防止迟到请求恢复旧设置。 |
| [重连只补发最近一条](#设备方向与协议-3) | 方向确认后，仅接收服务端授权的每设备投递。补发窗口由服务端配置，客户端不硬编码，也不拉取全局剪贴板绕过窗口。 |
| [持久化安全冲突恢复](#可选的安全冲突恢复) | 客户端可显式选择取消／跳过冲突，保留本机剪贴板，随后继续新复制；网络失败或重启后可继续恢复。 |
| [私有敏感收件箱](#私有敏感收件箱) | 将最近一条允许接收的已标记敏感文本保存在加密状态中，不写入系统剪贴板。关闭敏感同步后仍可接收普通文本。 |
| [后台接收接口](#后台接收) | 可完全不访问剪贴板而保存私有收件项，或显式启用不读取快照的剪贴板写入；调度与平台权限由宿主负责。 |
| [加密持久化状态](src/state.rs) | RCS1 AES-256-GCM 保存设备身份、待上传事务、接收游标、写入日志和恢复进度；重启后继续处理，以固定消息 ID 和确认抑制重试及回环。 |
| [元数据通知线程](src/worker.rs) | 长轮询、通知合并与连接退避，不把剪贴板正文复制到通知线程；提供显式停止／等待退出及标准错误链。 |

这些是共享库能力；客户端自行提供界面、权限和启用开关，新增接口不会自动为所有客户端开启对应功能。本仓库不提供桌面界面、服务端或端到端加密。

## 当前接口

全部网络操作使用 `/api/...`，并要求协议 **3**。这是一次明确的不兼容升级，服务端与客户端须配套更新；旧客户端继续使用旧版服务端。设备凭据与加密状态格式保持不变。Rust 桌面端和 Omaclipsync 消费本仓库的导出快照，接口逻辑只在本仓库修改并重新导出。

## 模块

| 模块 | 职责 |
| --- | --- |
| `direction` | 协议版本、方向／暂停 DTO、WebSocket 消息、ACK、结构化错误与变更校验 |
| `client` / `transport` | HTTPS 注册和数据操作、持久 WSS 方向控制及 HTTP 降级、取消、确认与长轮询；传输接口允许离线测试 |
| `state` | UUID、epoch、revision、outbox、接收游标、写入日志、敏感收件项和冲突恢复标记；兼容已有 RCS1 AES-256-GCM 状态 |
| `session` | 本地方向／暂停限制、离线最新值、固定 ID 重试、可选冲突恢复、敏感策略、前台／后台接收和写入前检查 |
| `worker` | 元数据长轮询、通知合并和连接退避；不复制剪贴板正文到轮询线程 |
| `Filter` | 上传/下载正则过滤 |
| `adapters` | 系统剪贴板、原子状态存储和可选历史接口 |

没有 Wayland、WinUI、Windows API、Secret Service、托盘或文件路径依赖。构造会话会加载或创建加密状态，但不联网、不读取或写入剪贴板。宿主在专用工作线程串行调用同步方法；不要从 UI 线程调用阻塞网络操作。

当前库版本 `0.4.0` 表示共享 Rust API 的版本，与服务端及客户端的正式产品版本独立。HTTP 统一使用 `/api/` 前缀，通过 `X-RCS-Protocol: 3` 协商最低协议版本。

## 适配边界

1. 用平台凭据库取得随机 32 字节密钥：Linux 使用 Secret Service，Windows 使用 DPAPI CurrentUser 保护密钥。
2. 实现 `StateStore`：限长读取、仅当前用户可读、原子持久化替换，完整保存核心生成的字节。只有文件不存在才能返回 `None`；解密失败不得偷偷重建身份。
3. 实现 `Clipboard`：返回文本及敏感标记；写入必须携带消息来源标记。`write` 成功表示系统剪贴板已接受内容，单独写入私有收件箱不能伪报 `applied`。
4. 使用 `NoHistory` 或实现 `HistorySink`。核心不决定历史存储格式，也不自行记录剪贴板正文。
5. 使用 `SessionConfig` 和 `Session::open` 构造会话，在 `check` 前调用 `set_sync_direction(SyncDirection::Bidirectional, false)` 或对应方向/暂停状态；默认未知方向禁止收发。监视器维护共享的本地事件计数；通过 `record_local` / `record_clear` 记录真实事件，通过 `flush` / `pull` 完成同步。

状态保存错误必须暂停并重建会话，不能继续用可能领先磁盘的内存状态。首次启动不能把原有系统剪贴板误当作新复制事件。通知只是唤醒信号，`state` 返回的 epoch 和 revision 才是同步依据。

同步入口会在发送前，将重启恢复的待发内容与当前剪贴板核对。`flush` 对最新待发内容重新应用当前敏感策略和正则过滤规则。新的超大复制也会淘汰旧内容；结果未知的上传仍通过取消接口确认后才能丢弃。剪贴板写入返回错误时，会话先持久化 `skipped` 再返回错误，让后续接收继续运行且不重放结果不明的写入；如果恢复状态保存失败，则按状态保存错误处理并重建会话。

`Error`、`SyncError` 和 `ClipboardError` 实现 `Display` 与 `std::error::Error`。显示文本不展开底层 URL 或适配器提供的详情，`source()` 保留可按类型检查的原始原因。`Worker::shutdown()` 只请求停止；`Worker::join()` 请求停止并等待退出，向宿主返回线程 panic，须在 UI 线程之外调用。直接丢弃 Worker 仍只请求停止而不等待。自定义 `Transport` 必须为每项操作（包括完整的长轮询请求）设置有限超时；停止请求不能中断正在执行的自定义调用。

## 协议和隐私约束

- 当前剪贴板仍带敏感标记时，关闭敏感同步不会阻塞普通文本接收。接收阶段的快照检查将策略拒绝视为没有可用文本，不读取被禁止的正文；真实剪贴板 I/O 故障仍会中止操作。关闭敏感同步后，远端标记文本仍被过滤。
- 对接 [RustClipSync 服务端](https://github.com/Infinifar/rustclipsync-server) 及其 [接口协议](https://github.com/Infinifar/rustclipsync-server/blob/main/PROTOCOL.md)，没有加入旧 SyncClipboard HTTP/SignalR 传输实现。
- UUID 标识一次复制事件；重试保留 UUID。revision 排序，epoch 隔离服务端重置；客户端时间仅用于重试有效期，不能用来排序或生成唯一性保证。
- 自动上传不静默覆盖冲突；离线只同步最新值。发送结果未知时保留原请求，先确认/取消后才处理新的最后一条。[安全冲突恢复](#可选的安全冲突恢复)须由宿主显式选择。
- 已接收消息通过持久化游标与确认避免重放。崩溃时无法判断是否成功写入的消息恢复为 `skipped`，不能承诺系统剪贴板副作用严格 exactly-once。
- 敏感内容由宿主传入标记，默认应关闭同步；正则和标记都无法识别全部敏感信息。
- TLS 默认校验证书，拒绝明文 HTTP、URL 内嵌凭据和重定向。兼容现有的显式 `insecure` 选项只供受控测试/自签名环境使用。
- 状态加密保护磁盘文件，不是端到端加密；服务端仍可读取同步正文。内存中的文本、密钥和配对密码目前不保证即时清零。
- 单条文本最大 10 MiB；状态最大 64 MiB。长轮询线程退出需要等待有限的方向协商和当前 HTTP 请求的 30 秒超时，宿主需避免反复创建重叠 worker。

## 设备方向与协议 3

`direction::SyncDirection` 提供 `PushOnly`、`ReceiveOnly`、`Bidirectional`，
序列化为 `PUSH_ONLY`、`RECEIVE_ONLY`、`BIDIRECTIONAL`。暂停是独立布尔工作态，
保留所选方向。未知、撤销、暂停均不接收；单向接收和暂停不能上传。
上报失败仍保留客户端本地新方向与过滤，不能因失败重新启用旧设置。

| 状态 | 上传 | 接收 |
| --- | --- | --- |
| 仅推送 | 允许 | 不允许 |
| 仅接收 | 不允许 | 允许 |
| 双向同步 | 允许 | 允许 |
| 暂停、未知或已撤销 | 不允许 | 不允许 |

宿主在后台线程串行调用以下接口，切换前增加本地 generation 并停止旧通知 Worker：

```rust,no_run
use rustclipsync_core::direction::SyncDirection;
// session.set_sync_direction(SyncDirection::Bidirectional, false)?;
// session.check()?; // 登记 -> 方向 ACK -> 元数据
// session.set_sync_direction(SyncDirection::ReceiveOnly, true)?; // 暂停
// session.revoke_sync_direction()?; // 显式退出登录，失败必须显示“未确认”
```

普通进程退出、网络断开和 `Worker::shutdown()` 不撤销方向，保留离线补发资格。
`re_pair_with_direction(direction, paused)` 先确认撤销旧会话再替换身份，撤销失败不删除
原身份。切换到暂停或单向接收时，旧的结果未知上传只能调用取消/结果查询，不重传正文。

HTTPS 客户端维护一个 `/api/ws` 持久控制连接，Bearer 和协议版本通过请求头传入，
不放入 URL。消息为 `SyncDirectionUpdate`、`SyncDirectionRevoke`，确认使用
`SyncDirectionAck`，错误使用 `SyncDirectionError`。帧最多 4096 字节，控制操作有
10 秒 I/O 总截止时间。控制锁不覆盖 HTTP 长轮询。HTTP 轮询上限 30 秒，首次发现/控制及降级可额外耗时
最多 40 秒，停止和 join 仍须在 UI 线程外执行。控制 socket 复用已认证 HTTPS 发现的
peer 地址，避免额外无截止时间的系统 DNS 调用。WS 不可用或确认丢失时，以相同
session/version/payload 通过 HTTP 重试，不盲目创建另一个会话；HTTP 也失败则保留未确认操作。

HTTP 降级和版本查询：

- `GET /api/devices/{deviceId}/sync-direction`：只读取当前版本，不激活方向、不刷新 TTL。
- 同一路径 `PUT`：`{sessionId,version,direction,paused,connect}`；新 UUID 会话使用
  `connect:true` 和 CAS=current+1，当前会话修改使用 `connect:false`。
- 同一路径 `DELETE`：`{sessionId,version}`，用于退出撤销。服务端保留版本墓碑，
  完全相同的操作可幂等重试，旧请求不能复活新会话。

```json
{"sessionId":"00000000-0000-4000-8000-000000000001","version":1,"direction":"BIDIRECTIONAL","paused":false,"connect":true}
```

全部同步响应须带 `X-RCS-Protocol: 3`。旧客户端被新服务端以 HTTP 426/
`PROTOCOL_UNSUPPORTED` 拒绝；新 core 也拒绝缺少版本头的旧服务端。错误码还包括
`UNAUTHENTICATED`、`DEVICE_MISMATCH`、`VERSION_STALE`、`SESSION_REVOKED`、
`SESSION_OFFLINE`、`RATE_LIMITED`、`INVALID_DIRECTION`、`DIRECTION_FORBIDDEN`。
被新会话取代的旧 Worker 停止，不自动重新声明会话恢复旧方向；由宿主显式连接/方向修改
重新开始。`SESSION_OFFLINE` 只表示当前在线租约过期，core 以新 GET+CAS 重连，
不盲重试旧数据请求；旧在途请求的错误不能重置或冻结新会话。普通数据请求带已确认的 `X-RCS-Sync-Session`，登记、方向控制及取消旧上传不需要。

重连先上报最新方向并等待 ACK，再由服务端发送管理页面窗口内、满足条件的每设备最近一条。
客户端不硬编码补发窗口，也不以全局当前消息绕过补发限制。窗口默认值、范围、零值及过期
规则以[服务端配置说明](https://github.com/Infinifar/rustclipsync-server/blob/main/docs/DIRECTION-PROTOCOL.md)
为准。方向 TTL 由服务端维护，上报/登录/活跃刷新；30 天未活跃撤销后须重新上报。

`RemoteState.revision` 仍是全局上传冲突版本；可空 `deliveryRevision` 是该设备获准接收
消息的版本。通知 Worker 独立保留单调的已观察全局版本，宿主较小的接收游标不能回退
它，避免被过滤元数据引起热轮询。合法补发即使早于全局版本也先处理，不能先跳过游标。无可接收消息时可前进已
观察版本，但不静默重设已排队上传的基准。正文端点仍检查当前方向、会话和设备授权。
持久化写入意图、skipped 确认、敏感私有收件箱和本地 generation 检查继续生效。

自定义 `Transport` 新增必需方法 `set_sync_direction`、`ensure_sync_direction`、
`revoke_sync_direction`；连同新 DTO、错误和 `delivery_revision` 字段，这是 crate 0.4.0
表示的 Rust API 破坏性变化。Android 保留独立 Kotlin 实现同一份协议，不强制加入 JNI。

## 私有敏感收件箱

显式开启已标记敏感文本同步后，客户端可调用 `Session::use_sensitive_inbox(true)`，
将收到的敏感正文和接收游标在同一次加密状态提交中保存。返回 `Outcome::Stored`，
服务端确认使用 `skipped`，不声称已写入系统剪贴板。只保留最近一项；
`clear_inbox(id)` 仅删除匹配项，不重置游标，清空后不会因此重新接收。

不启用私有收件箱时，前台接收遵循所选敏感策略与正常剪贴板写入流程。
关闭敏感同步时继续过滤远端标记文本，但本机已有敏感剪贴板不会阻塞普通文本接收。
`HistorySink::record_marked` 允许宿主将敏感正文排除出普通历史；通知身份不携带收件正文。

旧加密文件没有收件项或恢复标记时，分别默认为空和 false。状态上限为 64 MiB，
以容纳待发送项、替换项和收件项；单条文本仍限制为 10 MiB。

## 后台接收

| 接口 | 剪贴板访问 | 接收规则 |
| --- | --- | --- |
| `pull(observed, now)` | 读取快照，可写入 | 前台接收，检查快照与 generation |
| `pull_inbox(observed)` | 不读也不写 | 仅处理已标记敏感消息，要求启用加密收件箱；普通消息留待前台接收 |
| `pull_background(observed, true)` | 可写入，不读取 | 普通消息可写入，敏感消息继续遵守过滤／收件箱／写入策略 |

`pull_background(observed, false)` 等价于 `pull_inbox(observed)`。
所有接收路径继续执行方向、敏感与正则过滤、内容校验、持久化和确认检查。
配对及重启后的待发送校验须先通过前台 `check` 完成；待上传事务和冲突恢复仍留待前台处理。

后台写入须由宿主提供独立的显式启用开关。没有可读取的剪贴板快照时，只能依靠宿主的
generation 计数保护更新的本地复制，因此界面须说明可能覆盖其他应用刚复制的内容。
core 不授予后台运行时间、系统权限或调度保证。

## 可选的安全冲突恢复

自动上传默认仍返回 `SyncError::Conflict`，不会自行覆盖远端。宿主可显式选择安全跳过：

1. 在 `check` 后、普通接收前检查 `has_observed_upload_conflict()`；它也能识别重启恢复的、已尝试上传但基准过旧的待发事务。
2. 发现该状态或收到上传冲突后，在串行会话线程调用 `recover_upload_conflict(observed)`。
3. core 持久化恢复决定，通过取消接口确认旧上传结果，丢弃已排队复制，跳过当时有权接收的远端消息；保留本机剪贴板，不抬高冲突文本的基准版本、不重传该文本。取消不能回滚服务端已提交的上传。
4. 中断后，`flush` 会在网络恢复或重启后继续取消／跳过。宿主应丢弃恢复期间已观察到的监听事件，仅在恢复完成后接收新复制事件。

存储失败、epoch 改变和凭据错误继续停止同步，不按普通冲突处理。
Omaclipsync **0.4.0** 已选择此恢复策略；桌面客户端等其他适配仍保留各自的冲突策略。

## 开发和消费

```bash
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
cargo doc --locked --no-deps
```

已有服务端测试二进制时，可运行真实 HTTPS 协议验证：

```bash
python3 tests/server_e2e.py ../rustclipsync-server/target/release/rustclipsync-server
```

该脚本要求 Python 3 与 OpenSSL，启动临时服务端、数据库和仅监听 loopback 的 HTTPS 代理；剪贴板及密钥存储使用内存替身，不操作用户桌面。单元测试另包含由未修改的 Omaclipsync 0.2.0 代码生成的加密状态迁移样本。验证范围与平台限制见 [VALIDATION.md](VALIDATION.md)。

相邻仓库本地开发可以使用：

```toml
rustclipsync-core = { version = "=0.4.0", path = "../rustclipsync-core" }
```

源码仓库为 [Infinifar/rustclipsync-core](https://github.com/Infinifar/rustclipsync-core)，以 MIT 许可证公开开源，尚未发布 crates.io 包。为了让 Omaclipsync 的独立检出和源码发布包仍可编译，它消费 `core/vendor/rustclipsync-core` 的自动导出快照：

```bash
python3 tools/export_source.py ../omarchy-plugins-dev/io.github.infinifar.omaclipsync/core/vendor/rustclipsync-core
python3 tools/export_source.py ../omarchy-plugins-dev/io.github.infinifar.omaclipsync/core/vendor/rustclipsync-core --check
```

导出清单包含来源版本、原始提取提交及各文件 SHA-256。不要直接编辑消费端快照；修改本仓库、运行测试、重新导出，再更新消费端 Cargo.lock。将来发布共享库后可统一切换到固定版本或 Git 提交，不依赖浮动主分支。

Windows 与 Linux 桌面客户端为 [RustClipSync Desktop](https://github.com/Infinifar/rustclipsync-desktop)：Windows 11 使用 WinUI 3，Linux 使用独立的 Qt 6/QML 界面。Linux 提供面向 Debian 12、Ubuntu 24.04、Fedora 43 和 Arch/Omarchy 的 x86_64 安装包，无需 Omarchy 或 Quickshell。下载见 [Release 页面](https://github.com/Infinifar/rustclipsync-desktop/releases)，安装及桌面要求见 [Linux 使用说明](https://github.com/Infinifar/rustclipsync-desktop/blob/main/LINUX.md)。支持 X11；Wayland 需要 data-control 支持，缺少该协议的 GNOME/Wayland 会话暂不支持。

[Omaclipsync 插件](https://github.com/Infinifar/omaclipsync) 提供 Omarchy shell 集成。桌面客户端与插件均通过平台适配层复用本核心；Android 现阶段继续使用 Kotlin 实现同一协议，不强行加入 JNI。

MIT 许可，原始实现从 `Infinifar/omaclipsync` 的 Omaclipsync 0.2.0 抽取，
基线提交 `a8c330136886ad547dfadf386bb66901b3bbc45c`。
新增的跨平台接口、测试和导出工具在本项目维护。
