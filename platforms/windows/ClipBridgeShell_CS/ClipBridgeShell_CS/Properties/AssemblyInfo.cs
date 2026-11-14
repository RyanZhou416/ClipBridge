using System.Runtime.CompilerServices;

// 凭据存储的测试需要直接验证内部的 DPAPI 封装（例如“用错误 entropy 生成的 blob 必须读不出来”），
// 不为此把 DpapiProtector 暴露成公开 API。
[assembly: InternalsVisibleTo("ClipBridgeShell_CS.Tests.MSTest")]
