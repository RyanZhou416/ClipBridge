# 启动耗时测量

`measure-startup.ps1` 从外部测量“进程创建 → 主窗口就绪（主窗口句柄非 0 且标题非空）”，
不要求应用配合。默认使用 `CB_STARTUP_NO_FOREGROUND=1`（只传给被测进程），
窗口可见但不抢焦点；并会在每次测量后把前台窗口还给用户。恢复失败只告警，不中断测量。

> 测量默认使用 CB_STARTUP_NO_FOREGROUND=1，窗口可见但不抢焦点；并会在每次测量后把前台窗口还给用户。

## 只测一个构建

省略 `-ExeA` 时使用仓库内的默认 Release 外壳：

```powershell
pwsh -NoProfile -File scripts/measure-startup.ps1 -Runs 7
```

也可以显式指定 exe：

```powershell
pwsh -NoProfile -File scripts/measure-startup.ps1 `
  -ExeA 'platforms\windows\ClipBridgeShell_CS\ClipBridgeShell_CS\bin\x64\Release\net10.0-windows10.0.26100.0\win-x64\ClipBridgeShell_CS.exe' `
  -Runs 7
```

- `-Runs` 默认 7，最少 3；每个样本都重新启动进程。
- 每次启动前会结束残留的 `ClipBridgeShell_CS` / 测试宿主并等待 600 ms，
  避免单实例保护让新进程立即退出；清理失败会直接报错。
- 结束后默认结束应用；`-KeepAlive` 只保留最后一次成功启动的实例。
- 某次启动超时或提前退出会记为 `-1`，脚本继续跑完并最终以退出码 1 结束。
- 统计只使用有效样本；失败次数单独报告，存在失败时不要用这组数字下最终结论。

## 同一个 exe 的环境变量 A/B

`-EnvA` / `-EnvB` 接收哈希表，键和值都必须是字符串，只注入对应变体的子进程。
显式传 `-EnvB`（即使 `@{}`）会启用 A/B 模式；省略 `-ExeB` 时 B 复用 A 的路径。
在 PowerShell 会话中建议直接调用脚本，让哈希表保持为对象：

```powershell
# 对照「延迟首帧」：
./scripts/measure-startup.ps1 -Runs 7 -EnvA @{CB_SYNC_SHELL='1'} -EnvB @{}
```

如果需要从 cmd 或新开 `pwsh`，用 `-Command` 让 PowerShell 自己解析哈希表：

```powershell
pwsh -NoProfile -Command "& ./scripts/measure-startup.ps1 -Runs 7 -EnvA @{CB_SYNC_SHELL='1'} -EnvB @{}"
```

> 注意：在 PowerShell 中写成 `pwsh scripts/measure-startup.ps1 -EnvA @{...}` 时，
> 外层 PowerShell 会先把哈希表字符串化再传给原生的 `pwsh`，参数无法还原；
> 请使用上面的直接调用或 `-Command` 形式。

## A/B 两个构建

显式给两个不同的 exe（`-EnvA/-EnvB` 可省略）：

```powershell
pwsh -NoProfile -File scripts/measure-startup.ps1 `
  -ExeA '.\build\A\ClipBridgeShell_CS.exe' `
  -ExeB '.\build\B\ClipBridgeShell_CS.exe' `
  -Runs 9
```

启动顺序是 `A,B,A,B,...`，两个变体共享同一轮的热/IO/系统状态。重点看：

- `median delta (B-A)`：每对 A→B 的差值的中位数，负值表示 B 更快；
- `interleaved pairs`：逐对差值，用来判断方向是否稳定；
- `rough confidence`：只是基于正负号一致性的粗略提示，不是严格统计检验。

先用 `-DryRun` 校验路径并查看计划顺序（不启动、不杀进程、不写 CSV）：

```powershell
pwsh -NoProfile -File scripts/measure-startup.ps1 -Runs 7 -DryRun
```

`-Csv <path>` 会按 `timestamp,variant,run,window_ms,ws_mb` 追加，一行一次启动，便于跨会话积累。
`-TimeoutSec` 默认 20。若想观察正常抢焦点行为，可加 `-NoForeground:$false`（安全网仍生效）。

## 与应用内 trace 配合

设置 `CB_STARTUP_TRACE=1` 后启动，会写入：

```
%LOCALAPPDATA%\ClipBridge\startup.log
```

其中包含每阶段标记，例如 `app-ctor-enter`、`mainwindow-created`、`shellpage-created`、
`window-activated`、`mainpage-loaded`。适合定位慢在哪个阶段。

注意：trace 本身有开销，**端到端 A/B 数字必须在没有该环境变量时测**；
trace 只用于解释阶段耗时。

## 已知的 A/B 开关

其他 agent 正在加入以下环境变量开关：

- `CB_SYNC_SHELL=1`
- `CB_SYNC_MAINPAGE_LOADED=1`
- `CB_WIN_NO_MICA=1`
- `CB_WIN_NO_ICON=1`
- `CB_WIN_NO_PERSIST=1`

这些变量通过 `-EnvA` / `-EnvB` 注入时只影响对应变体，测量后父进程环境会恢复。
做 A/B 时只让被验证的开关不同，其余开关保持一致，否则比较的就不只是目标差异。
