using System.Runtime.CompilerServices;

// 测试需要直接验证内部实现，而不把测试专用 API 暴露成公开接口：
// - DPAPI 封装（例如“用错误 entropy 生成的 blob 必须读不出来”）；
// - 数据根/DLL 定位器/期望 ABI 注入（原生加载负路径与目录隔离用例）。
[assembly: InternalsVisibleTo("ClipBridgeShell_CS.Tests.MSTest")]
