using System.Runtime.InteropServices;
using System.Security.Cryptography;

namespace ClipBridgeShell_CS.Credentials;

/// <summary>
/// DPAPI（<c>CryptProtectData</c>/<c>CryptUnprotectData</c>）的最小封装。
/// </summary>
/// <remarks>
/// <list type="bullet">
/// <item>scope 固定 <c>CRYPTPROTECT_UI_FORBIDDEN</c>（CurrentUser + 禁止任何 UI 提示），
/// 与 Windows 默认的当前用户 DPAPI 密钥绑定；换用户/换机器无法解密，这是预期行为。</item>
/// <item>输入/输出 buffer 全部走非托管内存，并在 <see cref="Transform"/> 的 finally 里清零后释放；
/// 输出 buffer 由 DPAPI 用 <c>LocalAlloc</c> 分配，必须 <c>LocalFree</c>，不能 <c>FreeHGlobal</c>。</item>
/// <item>DLL 搜索路径限定 System32，避免被同名 DLL 劫持。</item>
/// </list>
/// </remarks>
internal static class DpapiProtector
{
    private const int CRYPTPROTECT_UI_FORBIDDEN = 0x1;

    [StructLayout(LayoutKind.Sequential)]
    private struct DataBlob
    {
        public int cbData;
        public IntPtr pbData;
    }

    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptProtectData(
        ref DataBlob pDataIn,
        string? szDataDescr,
        ref DataBlob pOptionalEntropy,
        IntPtr pvReserved,
        IntPtr pPromptStruct,
        int dwFlags,
        out DataBlob pDataOut);

    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptUnprotectData(
        ref DataBlob pDataIn,
        out IntPtr ppszDataDescr,
        ref DataBlob pOptionalEntropy,
        IntPtr pvReserved,
        IntPtr pPromptStruct,
        int dwFlags,
        out DataBlob pDataOut);

    [DllImport("kernel32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static extern IntPtr LocalFree(IntPtr hMem);

    public static byte[] Protect(byte[] plaintext, byte[] entropy) => Transform(plaintext, entropy, protect: true);

    public static byte[] Unprotect(byte[] ciphertext, byte[] entropy) => Transform(ciphertext, entropy, protect: false);

    private static byte[] Transform(byte[] input, byte[] entropy, bool protect)
    {
        ArgumentNullException.ThrowIfNull(input);
        ArgumentNullException.ThrowIfNull(entropy);

        var inputBlob = default(DataBlob);
        var entropyBlob = default(DataBlob);
        var outputBlob = default(DataBlob);
        var descriptionPtr = IntPtr.Zero;

        try
        {
            inputBlob = CreateBlob(input);
            entropyBlob = CreateBlob(entropy);

            var succeeded = protect
                ? CryptProtectData(
                    ref inputBlob,
                    null,
                    ref entropyBlob,
                    IntPtr.Zero,
                    IntPtr.Zero,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    out outputBlob)
                : CryptUnprotectData(
                    ref inputBlob,
                    out descriptionPtr,
                    ref entropyBlob,
                    IntPtr.Zero,
                    IntPtr.Zero,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    out outputBlob);

            if (!succeeded)
            {
                // 失败时不能信任 native 写回的 pDataOut（避免 LocalFree 垃圾指针）；
                // 不把 GetLastError 之外的任何内容带进异常消息；调用方只记录异常类型。
                outputBlob = default;
                throw new CryptographicException(Marshal.GetLastWin32Error());
            }

            var result = new byte[outputBlob.cbData];
            if (result.Length > 0)
            {
                Marshal.Copy(outputBlob.pbData, result, 0, outputBlob.cbData);
            }

            return result;
        }
        finally
        {
            // 输入可能是明文，entropy 与输出（解密方向是明文）也必须清零。
            FreeBlob(inputBlob);
            FreeBlob(entropyBlob);
            FreeOutputBlob(outputBlob);
            if (descriptionPtr != IntPtr.Zero)
            {
                LocalFree(descriptionPtr);
            }
        }
    }

    private static DataBlob CreateBlob(byte[] data)
    {
        if (data.Length == 0)
        {
            return default;
        }

        var buffer = Marshal.AllocHGlobal(data.Length);
        Marshal.Copy(data, 0, buffer, data.Length);
        return new DataBlob { cbData = data.Length, pbData = buffer };
    }

    private static void FreeBlob(DataBlob blob)
    {
        if (blob.pbData == IntPtr.Zero)
        {
            return;
        }

        ZeroMemory(blob.pbData, blob.cbData);
        Marshal.FreeHGlobal(blob.pbData);
    }

    private static void FreeOutputBlob(DataBlob blob)
    {
        if (blob.pbData == IntPtr.Zero)
        {
            return;
        }

        ZeroMemory(blob.pbData, blob.cbData);
        LocalFree(blob.pbData);
    }

    private static void ZeroMemory(IntPtr pointer, int length)
    {
        for (var i = 0; i < length; i++)
        {
            Marshal.WriteByte(pointer, i, 0);
        }
    }
}
