# ADR-006：Android 通知栏手动发送剪贴板

- 状态：**已决策，待实现与真机验收**。
- 日期：2026-11-17。
- 范围：Android 普通用户模式下，从手机当前剪贴板主动发送文本到其他设备的交互与访问路径。
- 实现参照：KDE Connect 的通知按钮、透明浮动 Activity 与窗口焦点回调，源码版本固定见 §8。
- 本记录仅确定产品与实现边界，不代表功能已完成，不恢复已暂停的 Android 开发、CI、打包或发布工作。

## 1. 背景

Android 10 起，普通后台应用不能仅凭常驻通知或前台服务读取系统剪贴板。读取资格与窗口焦点、默认输入法或系统特权有关；用户点击普通通知 Receiver，并不自动获得剪贴板读取权。

ClipBridge 需要降低普通用户的使用成本：不要求 Root、Shizuku、ADB 授权、不更换默认输入法，也不要求复制后进入应用首页或再次手工粘贴文本。

KDE Connect 已采用用户主动点击通知按钮，直接启动一个透明浮动 Activity；取得窗口焦点后读取，随后关闭。这是可参考的交互实现，不是后台权限被永久放开的证明。

## 2. 决策

**Android 默认发送路径采用“通知按钮 → 短暂透明 Activity → 获得焦点后读取 → 关闭 Activity → 后台完成发送”。**

产品目标是“一键发送当前剪贴板，不进入应用主界面”，不是“任何时候自动采集”，也不是“通知栏始终展开、完全没有窗口或焦点切换”。

| 项目 | 决定 |
|---|---|
| 用户入口 | 通知操作按钮“发送剪贴板” |
| 触发方式 | 每次由用户明确点击；不通过轮询或日志监听自动弹出 Activity |
| 读取方式 | 获得焦点的短暂 Activity 使用公开 `ClipboardManager` API |
| 普通模式前置 | 完成现有账号/连接配置并允许通知；读取还须满足系统/厂商实际权限条件 |
| 不作为前置 | Root、Shizuku、ADB、READ_LOGS、悬浮窗权限、无障碍权限、默认输入法身份 |
| 首期内容 | 当前剪贴板中的文本；不以文本成功宣称图片、文件 URI 或厂商历史面板已支持 |
| 现有 Shizuku 实现 | 不是普通模式必经入口；是否保留为高级自动同步选项另行决定，本 ADR 不要求立即删除 |
| RemoteInput | 不作为默认发送方式；不要求用户在通知输入框中再次粘贴 |

## 3. 用户体验与能力边界

```text
用户在任意应用复制文本
    ↓
拉下通知栏，点击“发送剪贴板”
    ↓
短暂透明 Activity 获得焦点并读取一次文本
    ↓
保存本次发送快照并关闭 Activity，不等待网络完成
    ↓
交给现有核心/发送任务处理
    ↓
通知显示发送状态或失败原因
```

### 3.1 承诺的行为

- 不打开 ClipBridge 首页、历史列表或完整设置界面。
- 正常发送不要求再次手工粘贴，不弹键盘，也不把原输入法换成 ClipBridge。
- 读取窗口不调暗背景、不停留等待网络，不出现在最近任务中。
- 原应用不被关闭；短暂 Activity 完成后恢复系统原有任务栈。
- 发送范围遵守现有账号、配对与同步设置，通知中的目标说明与实际范围一致；不因增加此按钮扩大接收范围。本决策不新增设备选择器或改变配对模型。

### 3.2 不作出的承诺

- 通知栏始终保持展开；系统可能因启动 Activity 而收起通知栏。
- 完全不切换窗口焦点、所有设备零闪动或动画完全一致。
- 锁屏时可任意读取、厂商剪贴板权限被拒后仍可读取。
- 全天候自动监听、永不被杀、通知永远不可划掉或无需重新连接。
- 所有原生 Android、HyperOS 版本以及多用户/工作资料场景已兼容。

因此，对外应称“从通知一键发送，无需打开首页”，不能称为“纯后台读取”或“绝不离开通知栏”。

## 4. 实现约束

### 4.1 通知与读取窗口

- 通知动作使用明确指向内部 Activity 的 `PendingIntent.getActivity()`，根据目标系统正确设置 PendingIntent flags；不经过 Receiver/Service 再跳转 Activity 的通知跳板路径。
- Activity 应非外部导出、从最近任务排除、透明且不调暗背景；不要依靠额外悬浮窗授权，也不要使用 `NoDisplay` 或不可获焦窗口代替需要焦点的 Activity。
- 在 `onWindowFocusChanged(true)` 等真实焦点信号到达后读取，不能只靠 `onCreate()`、`onResume()` 或固定延时假设已经有读取资格。
- 每次用户请求只捕获一次。重复焦点回调、窗口重建或快速多次点击不能让同一次请求重复发送。
- 无法获焦或操作取消时有界退出，不循环抢焦点，不留下透明窗口阻塞用户交互。不得自动展开通知栏来掩饰一次真实窗口切换。

### 4.2 读取、任务交接与状态

- 使用本次点击后实际读取的文本快照；空剪贴板、无文本、权限拒绝或读取异常均明确失败，不以旧缓存冒充当前内容。
- 首期直接处理文本项；不能通过任意 `content://` URI 的隐式读取，把图片/文件转换问题隐藏在 `coerceToText` 中。
- 确认请求与文本快照已交给应用拥有的任务后，尽快关闭 Activity。网络连接、Rust 核心初始化或内容发送不能占住焦点窗口等待。
- 核心未就绪或目标离线时明确显示“未发送/连接失败”等状态；若以后增加排队功能，应单独定义保留时限、目标和取消语义，不在重新联网时意外发送陈旧敏感内容。
- “已读取”“已交给发送任务”“发送中”“对方已收到”必须区分。只有协议层有实际送达证据才能显示“对方已收到”；读取成功或调用 ingest 成功不是送达证明。
- 本决策不新增跨平台协议或 Rust FFI。复用现有核心能力；如果尚不能表达目标选择或送达状态，应如实限制界面，不虚构确认事件。

### 4.3 隐私与授权

- 首次开启该功能时明确说明：点击会读取当时剪贴板中的文本并发送给指定设备。
- 通知与常规日志不展示或打印剪贴板正文、密码、验证码等敏感内容；锁屏内容使用私密展示策略，发送动作要求解锁。
- 沿用既有敏感内容与同步策略，不因“手动发送”绕过核心限制。保留并处理系统敏感标记；如果策略要求确认，应使用明确可见的交互，不把透明窗口当作确认。
- 通知权限、厂商剪贴板权限被拒时，不反复索权或静默切换到特权路径；用户可以关闭通知入口。
- KDE Connect 源码带 GPL 许可。借鉴公开机制与行为，不在未核对许可兼容性时直接复制实现到本项目。

## 5. 后台连接与接收方向

### 5.1 后台连接不等于剪贴板读取权

通知卡片只是入口；显示通知不能证明进程仍存活或与设备仍连接。通知状态须基于实际连接/任务状态，不能无条件显示“已连接”。不使用假音乐播放、静音音频或媒体会话来维持常驻。

当前服务使用 `dataSync`。KDE Connect 使用 `connectedDevice`，Android 官方也将通过网络与外部设备交互列为该类型用途。后续恢复实施时，**优先按真实跨设备连接用途评估 `connectedDevice`，确认权限、启动条件和应用商店政策后再调整**；不能仅为规避 `dataSync` 时限而改一个类型名，也不保证该类型免受厂商后台管理。

### 5.2 手机接收不与发送混为一谈

本 ADR 的核心决策是“手机 → 其他设备”的手动读取发送。其他设备 → 手机的接收与写入是另一条路径：AOSP 对写入不要求与读取完全相同的焦点条件，但厂商行为、网络与进程存活仍需验证。

未来接收入口可以在通知中提供反馈或操作，但不得用接收写入成功证明发送读取已可用；也不得由本 ADR 推导出全天无感双向同步的承诺。

## 6. 现有代码与待实施差异

| 当前位置 | 当前行为 | 后续实施要求 |
|---|---|---|
| [MainActivity](<../../platforms/android/ClipBridgeShellAndroid/app/src/main/java/com/ryan416/clipbridgeshellandroid/MainActivity.java>) | 启动服务前检查 Shizuku，授权流程与普通启动耦合 | 普通手动发送不再被 Shizuku 是否运行/授权所阻断 |
| [ForegroundService](<../../platforms/android/ClipBridgeShellAndroid/app/src/main/java/com/ryan416/clipbridgeshellandroid/service/ForegroundService.java>) | 常驻通知显示 Shizuku 模式，使用 dataSync，自动启动 watcher | 增加用户主动发送入口与真实状态；普通模式不自动启动特权 watcher；另行落实合规的后台任务/连接生命周期 |
| [ClipboardWatcher](<../../platforms/android/ClipBridgeShellAndroid/app/src/main/java/com/ryan416/clipbridgeshellandroid/clipboard/ClipboardWatcher.java>) | 通过 Shizuku 注册监听、读取并 ingest | 用户点击捕获与自动监听职责分开；发送复用核心入口，但不把 Shizuku 作为默认捕获器 |
| [ShizukuClipboardManager](<../../platforms/android/ClipBridgeShellAndroid/app/src/main/java/com/ryan416/clipbridgeshellandroid/shizuku/ShizukuClipboardManager.java>) | 以 Shell 身份调用隐藏 Clipboard Binder 接口 | 不供普通通知点击路径使用；高级模式是否保留另行决定 |
| [ClipboardApplyService](<../../platforms/android/ClipBridgeShellAndroid/app/src/main/java/com/ryan416/clipbridgeshellandroid/clipboard/ClipboardApplyService.java>) | 接收文本后通过 Shizuku 写入系统剪贴板 | 普通模式的接收写入需独立适配，不能只解除发送侧授权却继续依赖特权接收 |
| [AndroidManifest](<../../platforms/android/ClipBridgeShellAndroid/app/src/main/AndroidManifest.xml>) | 尚无上述透明捕获 Activity；声明 dataSync 与 Shizuku provider | 后续增加内部捕获 Activity，并按实际服务类型同步权限；本次不修改 |

上述差异是实施清单，不是本轮代码变更。现有代码仍处于旧路径，不能向用户表示默认模式已经无需 Shizuku。

## 7. 后续验收条件（待恢复实施后执行）

- [ ] 不安装或不启动 Shizuku、无 Root、无 ADB 授权、默认键盘保持不变，仍能使用通知手动发送。
- [ ] 在其他应用复制文本后点击通知动作，实际读取本次文本；不进入首页、不要求粘贴、无键盘弹出。
- [ ] 读取等待真实焦点，完成/失败/取消后透明 Activity 均退出；不为网络传输长期持有窗口。
- [ ] 快速连点、重复焦点回调、旋转/重建不导致同次请求重复发送。
- [ ] 空剪贴板、非文本、锁屏、剪贴板权限拒绝、通知权限关闭、目标离线、核心冷启动均有正确结果，不误报送达。
- [ ] 发往的是用户已确认的目标设备；系统敏感标记、通知隐私和日志不泄露正文的规则得到落实。
- [ ] 分别记录原生 Android 和具体 HyperOS 设备的型号、Android/ROM 完整版本，以及通知栏是否收起、是否闪动、是否回到原应用；不由单一机型结果推断全部兼容。
- [ ] 若调整前台服务类型，独立验证启动限制、权限、后台/重启恢复和电池管理；不把读取成功视作保活成功。
- [ ] 接收写入与发送读取分别验收；文本可用不代表图片/文件已经支持。

当前未执行这些新路径的真机验证；文档记录不触发 CI、构建、手机权限修改或功能实现。

## 8. 依据

### 成熟实现：固定源码版本

- [KDE Connect 通知发送按钮](https://github.com/KDE/kdeconnect-android/blob/ae93fa00d0d20904f841ff6864c12585c234fba8/src/main/java/org/kde/kdeconnect/BackgroundService.kt#L203-L208)：通知动作直接通过 PendingIntent 启动读取 Activity。
- [ClipboardFloatingActivity](https://github.com/KDE/kdeconnect-android/blob/ae93fa00d0d20904f841ff6864c12585c234fba8/src/main/java/org/kde/kdeconnect/plugins/clipboard/ClipboardFloatingActivity.kt)：取得窗口焦点后读取并关闭；手动触发不要求 READ_LOGS。
- [透明浮动主题](https://github.com/KDE/kdeconnect-android/blob/ae93fa00d0d20904f841ff6864c12585c234fba8/src/main/res/values/styles.xml) 与 [Activity 声明](https://github.com/KDE/kdeconnect-android/blob/ae93fa00d0d20904f841ff6864c12585c234fba8/src/main/AndroidManifest.xml#L203-L208)：透明、不调暗、排除最近任务。
- [快捷磁贴路径](https://github.com/KDE/kdeconnect-android/blob/ae93fa00d0d20904f841ff6864c12585c234fba8/src/main/java/org/kde/kdeconnect/plugins/clipboard/ClipboardTileService.kt)：显式使用 startActivityAndCollapse，说明“不进入首页”不等于“通知/快捷设置面板永不收起”；不据此新增本项目磁贴需求。

### 平台规则

- [Android 10 剪贴板访问限制](https://developer.android.google.cn/about/versions/10/privacy/changes?hl=en#clipboard-data)。
- [Android 12 通知跳板限制](https://developer.android.google.cn/about/versions/12/behavior-changes-12?hl=en#notification-trampolines)。
- [前台服务类型：connectedDevice](https://developer.android.google.cn/develop/background-work/services/fgs/service-types?hl=en#connected-device)。
- [Android 15 dataSync 时限](https://developer.android.google.cn/about/versions/15/behavior-changes-15?hl=en#fgs-timeout)。
- [小米：读取剪贴板权限使用标准](https://dev.mi.com/xiaomihyperos/documentation/detail?pId=2225)：普通后台读取不应被当作已授权能力，具体 ROM 行为仍需验证。
