using System.Security.Cryptography;
using System.Text;

namespace ClipBridgeShell_CS.Helpers;

/// <summary>密码强度等级。仅用于提示；只有 <see cref="PasswordWeakness.None"/> 之外的结果才会硬性阻止创建/修改。</summary>
public enum PasswordStrength
{
    Weak,
    Ok,
    Strong,
}

/// <summary>创建/修改密码被拒绝的原因（登录已有账号时不使用本策略）。</summary>
public enum PasswordWeakness
{
    None,
    TooShort,
    SameAsUsername,
    CommonPattern,
    LowEntropy,
}

/// <summary>密码评估结果。</summary>
public readonly record struct PasswordStrengthResult(
    PasswordStrength Strength,
    PasswordWeakness Weakness,
    double EntropyBits)
{
    /// <summary>是否达到创建/修改账号的最低要求。</summary>
    public bool IsAcceptable => Weakness == PasswordWeakness.None;
}

/// <summary>
/// 账号密码强度策略（纯函数，便于单元测试）。
///
/// 背景（审计 F3）：OPAQUE 的 ServerSetup 由共享账号密码确定性派生，被动监听者可以
/// 对每个候选密码做离线验证。项目保留“账号 + 密码”的配对模型、不引入带外配对码，
/// 因此抗离线猜测的关键就是密码本身的熵。这里在“创建/修改”路径上强制最低强度；
/// 已有账号的登录/短密码仍由 core 兼容（core 侧只告警，见 cb_core_ffi::marshal）。
/// </summary>
public static class PasswordStrengthPolicy
{
    /// <summary>创建/修改密码的最小长度。</summary>
    public const int MinimumLength = 12;

    /// <summary>粗估熵下限（bit）。12 位纯小写随机串约 56 bit，因此会被要求增加字符类别。</summary>
    public const double MinimumEntropyBits = 60.0;

    /// <summary>“强”等级的熵下限（bit）。</summary>
    public const double StrongEntropyBits = 80.0;

    /// <summary>生成器输出的随机位数（Crockford base32 每字符 5 bit）。</summary>
    public const int GeneratedSecretBits = 160;

    /// <summary>生成器输出的符号数（160 / 5 = 32）。</summary>
    public const int GeneratedSecretSymbolCount = 32;

    /// <summary>生成器的分组大小（每 4 个字符插入一个 '-'）。</summary>
    public const int GeneratedSecretGroupSize = 4;

    /// <summary>生成器使用的 Crockford base32 字母表：去掉容易混淆的 I、L、O、U。</summary>
    public const string GeneratedSecretAlphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

    /// <summary>生成器输出中的分隔符。</summary>
    public const char GeneratedSecretSeparator = '-';

    // 常见弱密码片段（比较前会去掉分隔符并转小写）。命中即判定为“常见/规律密码”。
    private static readonly string[] s_commonPasswordFragments =
    {
        "password",
        "passw0rd",
        "123456",
        "12345678",
        "123456789",
        "1234567890",
        "qwerty",
        "qwertyuiop",
        "letmein",
        "welcome",
        "admin",
        "abc123",
        "iloveyou",
        "monkey",
        "dragon",
        "sunshine",
        "princess",
        "football",
        "master",
        "secret",
        "changeme",
        "default",
        "root",
    };

    /// <summary>
    /// 评估候选密码。返回的 <see cref="PasswordStrengthResult.Weakness"/> 为拒绝原因；
    /// 只有 <see cref="PasswordWeakness.None"/> 才允许创建/修改账号。
    /// </summary>
    public static PasswordStrengthResult Evaluate(string? password, string? username)
    {
        password ??= string.Empty;
        var trimmed = password.Trim();

        if (password.Length < MinimumLength)
        {
            return new PasswordStrengthResult(
                PasswordStrength.Weak,
                PasswordWeakness.TooShort,
                EstimateEntropyBits(password));
        }

        if (IsSameAsUsername(trimmed, username))
        {
            return new PasswordStrengthResult(
                PasswordStrength.Weak,
                PasswordWeakness.SameAsUsername,
                EstimateEntropyBits(password));
        }

        if (IsCommonOrSequential(trimmed))
        {
            return new PasswordStrengthResult(
                PasswordStrength.Weak,
                PasswordWeakness.CommonPattern,
                EstimateEntropyBits(password));
        }

        var entropy = EstimateEntropyBits(password);
        if (entropy < MinimumEntropyBits)
        {
            return new PasswordStrengthResult(PasswordStrength.Weak, PasswordWeakness.LowEntropy, entropy);
        }

        var strength = entropy >= StrongEntropyBits ? PasswordStrength.Strong : PasswordStrength.Ok;
        return new PasswordStrengthResult(strength, PasswordWeakness.None, entropy);
    }

    /// <summary>是否属于“原账号原密码”的重复登录（重复登录不套用创建/修改强度门槛）。</summary>
    public static bool IsExistingCredentialReuse(
        string? existingUsername,
        string? existingPassword,
        string? username,
        string? password)
    {
        return !string.IsNullOrEmpty(existingUsername)
            && !string.IsNullOrEmpty(existingPassword)
            && string.Equals(existingUsername, username, StringComparison.OrdinalIgnoreCase)
            && string.Equals(existingPassword, password, StringComparison.Ordinal);
    }

    /// <summary>
    /// 生成一个 160-bit 随机账号密钥，使用 Crockford base32（无 I/L/O/U）并按 4 位分组，
    /// 例如 <c>7Q3M-9XKP-2VWZ-...</c>。熵由 <see cref="RandomNumberGenerator"/> 提供。
    /// </summary>
    public static string GenerateStrongPassword()
    {
        // 160-bit 随机串几乎不可能命中弱模式，但生成物必须能通过自己的策略
        // （例如极小概率出现 "ABCD" 这样的连续 4 字符），否则用户点“创建”会被拒。
        string candidate;
        do
        {
            candidate = GenerateCandidate();
        }
        while (!Evaluate(candidate, username: null).IsAcceptable);

        return candidate;
    }

    private static string GenerateCandidate()
    {
        var builder = new StringBuilder(
            GeneratedSecretSymbolCount + (GeneratedSecretSymbolCount / GeneratedSecretGroupSize) - 1);

        for (var i = 0; i < GeneratedSecretSymbolCount; i++)
        {
            if (i > 0 && i % GeneratedSecretGroupSize == 0)
            {
                builder.Append(GeneratedSecretSeparator);
            }

            var index = RandomNumberGenerator.GetInt32(GeneratedSecretAlphabet.Length);
            builder.Append(GeneratedSecretAlphabet[index]);
        }

        return builder.ToString();
    }

    /// <summary>
    /// 按“字符类别池 × 长度”粗估熵（bit）。这是刻意保守的近似：
    /// 不建模字典/重复/连续模式，这些由 <see cref="IsCommonOrSequential"/> 单独拦截。
    /// </summary>
    public static double EstimateEntropyBits(string? password)
    {
        if (string.IsNullOrEmpty(password))
        {
            return 0;
        }

        var hasLower = false;
        var hasUpper = false;
        var hasDigit = false;
        var hasSymbol = false;
        var hasOtherLetter = false;

        foreach (var ch in password)
        {
            if (char.IsLower(ch))
            {
                hasLower = true;
            }
            else if (char.IsUpper(ch))
            {
                hasUpper = true;
            }
            else if (char.IsDigit(ch))
            {
                hasDigit = true;
            }
            else if (char.IsSymbol(ch) || char.IsPunctuation(ch) || char.IsWhiteSpace(ch))
            {
                hasSymbol = true;
            }
            else if (char.IsLetter(ch))
            {
                hasOtherLetter = true;
            }
        }

        var poolSize = (hasLower ? 26 : 0)
            + (hasUpper ? 26 : 0)
            + (hasDigit ? 10 : 0)
            + (hasSymbol ? 33 : 0)
            + (hasOtherLetter ? 100 : 0);

        if (poolSize <= 1)
        {
            return 0;
        }

        return password.Length * Math.Log2(poolSize);
    }

    private static bool IsSameAsUsername(string trimmedPassword, string? username)
    {
        var trimmedUsername = username?.Trim();
        return !string.IsNullOrEmpty(trimmedUsername)
            && string.Equals(trimmedPassword, trimmedUsername, StringComparison.OrdinalIgnoreCase);
    }

    private static bool IsCommonOrSequential(string password)
    {
        if (IsAllSameCharacter(password) || IsRepeatedBlock(password))
        {
            return true;
        }

        if (HasSequentialRun(password, minimumRunLength: 4))
        {
            return true;
        }

        var normalized = NormalizeForCommonPasswordCheck(password);
        foreach (var fragment in s_commonPasswordFragments)
        {
            if (normalized.Contains(fragment, StringComparison.Ordinal))
            {
                return true;
            }
        }

        return false;
    }

    private static bool IsAllSameCharacter(string password)
    {
        for (var i = 1; i < password.Length; i++)
        {
            if (char.ToLowerInvariant(password[i]) != char.ToLowerInvariant(password[0]))
            {
                return false;
            }
        }

        return password.Length > 0;
    }

    private static bool IsRepeatedBlock(string password)
    {
        var length = password.Length;
        for (var period = 1; period <= length / 2; period++)
        {
            if (length % period != 0)
            {
                continue;
            }

            var repeats = true;
            for (var i = period; i < length; i++)
            {
                if (password[i] != password[i % period])
                {
                    repeats = false;
                    break;
                }
            }

            if (repeats)
            {
                return true;
            }
        }

        return false;
    }

    private static bool HasSequentialRun(string password, int minimumRunLength)
    {
        if (minimumRunLength < 2)
        {
            minimumRunLength = 2;
        }

        var ascendingRun = 1;
        var descendingRun = 1;

        for (var i = 1; i < password.Length; i++)
        {
            var delta = password[i] - password[i - 1];
            ascendingRun = delta == 1 ? ascendingRun + 1 : 1;
            descendingRun = delta == -1 ? descendingRun + 1 : 1;

            if (ascendingRun >= minimumRunLength || descendingRun >= minimumRunLength)
            {
                return true;
            }
        }

        return false;
    }

    private static string NormalizeForCommonPasswordCheck(string password)
    {
        var builder = new StringBuilder(password.Length);
        foreach (var ch in password)
        {
            if (char.IsLetterOrDigit(ch))
            {
                builder.Append(char.ToLowerInvariant(ch));
            }
        }

        return builder.ToString();
    }
}
