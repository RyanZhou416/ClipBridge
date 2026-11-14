using ClipBridgeShell_CS.Core.Services;

namespace ClipBridgeShell_CS.Services;

/// <summary>
/// 数据根目录提供者（DI 包装）。服务不直接调用静态
/// <see cref="CoreConfigBuilder.ResolveDataRootInfo"/>，以便测试注入便携/默认解析结果。
/// </summary>
public interface IDataRootProvider
{
    /// <summary>当前进程的数据根目录解析结果（与 core 使用同一份缓存，不会分裂）。</summary>
    DataRootResolution Current
    {
        get;
    }
}

/// <summary>
/// 生产实现：委托给 <see cref="CoreConfigBuilder.ResolveDataRootInfo"/>（进程内按 appName 缓存）。
/// </summary>
public sealed class DefaultDataRootProvider : IDataRootProvider
{
    public DataRootResolution Current => CoreConfigBuilder.ResolveDataRootInfo();
}
