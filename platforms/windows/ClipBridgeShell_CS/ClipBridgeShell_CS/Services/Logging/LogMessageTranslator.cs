using System.Collections.Generic;

namespace ClipBridgeShell_CS.Services.Logging;

/// <summary>
/// 日志消息翻译器，提供中英文翻译映射
/// </summary>
public static class LogMessageTranslator
{
    private static readonly Dictionary<string, (string En, string ZhCn)> _translations = new()
    {
        // 初始化相关
        ["CoreHostService initializing..."] = ("CoreHostService initializing...", "核心主机服务正在初始化..."),
        ["CoreHostService initialization failed"] = ("CoreHostService initialization failed: {0}", "核心主机服务初始化失败: {0}"),
        ["Core DLL loaded from"] = ("Core DLL loaded from: {0}", "核心 DLL 已从以下路径加载: {0}"),
        ["Core DLL not found"] = ("Core DLL not found, Core will run in degraded mode", "核心 DLL 未找到，核心将在降级模式下运行"),
        ["Core initialized successfully"] = ("Core initialized successfully, handle: {0}", "核心初始化成功，句柄: {0}"),
        ["Core initialization failed"] = ("Core initialization failed: {0}", "核心初始化失败: {0}"),

        // 剪贴板相关
        ["Clipboard monitoring started"] = ("Clipboard monitoring started", "剪贴板监听已启动"),
        ["Clipboard monitoring stopped"] = ("Clipboard monitoring stopped", "剪贴板监听已停止"),
        ["Clipboard content changed"] = ("Clipboard content changed, processing...", "剪贴板内容已变化，正在处理..."),
        ["Clipboard ingest allowed"] = ("Clipboard ingest allowed, type: {0}", "剪贴板内容摄入已允许，类型: {0}"),
        ["Clipboard ingest denied"] = ("Clipboard ingest denied by policy: {0}", "剪贴板内容摄入被策略拒绝: {0}"),
        ["Failed to process clipboard change"] = ("Failed to process clipboard change: {0}", "处理剪贴板变化失败: {0}"),

        // 事件处理
        ["Event enqueued"] = ("Event enqueued: type={0}, queue_length={1}", "事件已入队: 类型={0}，队列长度={1}"),
        ["Processing event"] = ("Processing event: type={0}, json_length={1}", "正在处理事件: 类型={0}，JSON长度={1}"),
        ["Failed to parse event JSON"] = ("Failed to parse event JSON: {0}", "事件JSON解析失败: {0}"),
        ["Peer event received"] = ("Peer event received: type={0}, device_id={1}, is_online={2}", "对等设备事件已接收: 类型={0}，设备ID={1}，在线={2}"),
        ["Content cached event received"] = ("Content cached event received: item_id={0}, file_id={1}", "内容缓存事件已接收: 项目ID={0}，文件ID={1}"),
        ["Failed to dispatch event"] = ("Failed to dispatch event: {0}, error: {1}", "事件分发失败: {0}，错误: {1}"),

        // 网络状态
        ["Peer added/updated"] = ("Peer added/updated: device_id={0}, name={1}, is_online={2}, last_seen={3}", "对等设备已添加/更新: 设备ID={0}，名称={1}，在线={2}，最后可见={3}"),
        ["Querying peer list"] = ("Querying peer list, found {0} peers", "正在查询对等设备列表，发现 {0} 个设备"),

        // 核心降级
        ["Core degraded"] = ("Core degraded: {0}", "核心已降级: {0}"),
    };

    /// <summary>
    /// 获取消息的英文版本
    /// </summary>
    public static string GetEnglish(string message)
    {
        // 如果消息包含格式化占位符，尝试从字典中查找
        // 简化处理：直接返回原消息（因为字典中的键是完整消息）
        if (_translations.TryGetValue(message, out var translation))
        {
            return translation.En;
        }
        // 如果找不到，返回原消息
        return message;
    }

    /// <summary>
    /// 获取消息的中文版本
    /// </summary>
    public static string? GetChinese(string message)
    {
        if (_translations.TryGetValue(message, out var translation))
        {
            return translation.ZhCn;
        }
        return null;
    }

    /// <summary>
    /// 格式化消息（支持占位符）
    /// </summary>
    public static (string En, string? ZhCn) GetTranslated(string message, params object[] args)
    {
        if (_translations.TryGetValue(message, out var translation))
        {
            try
            {
                var en = args.Length > 0 ? string.Format(translation.En, args) : translation.En;
                var zhCn = args.Length > 0 ? string.Format(translation.ZhCn, args) : translation.ZhCn;
                return (en, zhCn);
            }
            catch
            {
                // 格式化失败，返回未格式化的版本
                return (translation.En, translation.ZhCn);
            }
        }
        // 如果找不到翻译，尝试格式化原消息
        try
        {
            var formatted = args.Length > 0 ? string.Format(message, args) : message;
            return (formatted, null);
        }
        catch
        {
            return (message, null);
        }
    }
}
