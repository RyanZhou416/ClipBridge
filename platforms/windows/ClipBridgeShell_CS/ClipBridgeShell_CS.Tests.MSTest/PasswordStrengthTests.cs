using ClipBridgeShell_CS.Helpers;

namespace ClipBridgeShell_CS.Tests.MSTest;

/// <summary>
/// 审计 F3 的壳侧缓解：创建/修改账号时必须使用高熵密码；生成器输出 160-bit 的
/// Crockford base32 密钥。这里覆盖纯策略函数，不依赖 UI/WinUI 运行时。
/// </summary>
[TestClass]
public class PasswordStrengthTests
{
    [TestMethod]
    public void Evaluate_RejectsTooShort()
    {
        var result = PasswordStrengthPolicy.Evaluate("Short1!aaaa", "alice");

        Assert.AreEqual(PasswordWeakness.TooShort, result.Weakness);
        Assert.AreEqual(PasswordStrength.Weak, result.Strength);
        Assert.IsFalse(result.IsAcceptable);
    }

    [TestMethod]
    public void Evaluate_RejectsAllSameCharacter()
    {
        var result = PasswordStrengthPolicy.Evaluate("aaaaaaaaaaaaaaaa", "alice");

        Assert.AreEqual(PasswordWeakness.CommonPattern, result.Weakness);
        Assert.IsFalse(result.IsAcceptable);
    }

    [TestMethod]
    public void Evaluate_RejectsRepeatedBlock()
    {
        var result = PasswordStrengthPolicy.Evaluate("abcabcabcabc", "alice");

        Assert.AreEqual(PasswordWeakness.CommonPattern, result.Weakness);
        Assert.IsFalse(result.IsAcceptable);
    }

    [TestMethod]
    public void Evaluate_RejectsSequentialLetters()
    {
        var result = PasswordStrengthPolicy.Evaluate("abcdefghijkl", "alice");

        Assert.AreEqual(PasswordWeakness.CommonPattern, result.Weakness);
        Assert.IsFalse(result.IsAcceptable);
    }

    [TestMethod]
    public void Evaluate_RejectsCommonPassword()
    {
        Assert.AreEqual(
            PasswordWeakness.CommonPattern,
            PasswordStrengthPolicy.Evaluate("password1234", "alice").Weakness);
        Assert.AreEqual(
            PasswordWeakness.CommonPattern,
            PasswordStrengthPolicy.Evaluate("123456789012", "alice").Weakness);
    }

    [TestMethod]
    public void Evaluate_RejectsPasswordEqualToAccountName()
    {
        var result = PasswordStrengthPolicy.Evaluate("SuperLongUserName", " superlongusername ");

        Assert.AreEqual(PasswordWeakness.SameAsUsername, result.Weakness);
        Assert.IsFalse(result.IsAcceptable);
    }

    [TestMethod]
    public void Evaluate_RejectsLowEntropyDigitOnlyPassword()
    {
        var result = PasswordStrengthPolicy.Evaluate("837492615038", "alice");

        Assert.AreEqual(PasswordWeakness.LowEntropy, result.Weakness);
        Assert.IsTrue(result.EntropyBits < PasswordStrengthPolicy.MinimumEntropyBits);
        Assert.IsFalse(result.IsAcceptable);
    }

    [TestMethod]
    public void Evaluate_AcceptsMixedTwelveCharacterPassword()
    {
        var result = PasswordStrengthPolicy.Evaluate("Tr0ub4dor&3x", "alice");

        Assert.AreEqual(PasswordWeakness.None, result.Weakness);
        Assert.AreEqual(PasswordStrength.Ok, result.Strength);
        Assert.IsTrue(result.IsAcceptable);
    }

    [TestMethod]
    public void Evaluate_AcceptsLongMixedPasswordAsStrong()
    {
        var result = PasswordStrengthPolicy.Evaluate("V7!qZ2@wL9#pR4$tY6&u", "alice");

        Assert.AreEqual(PasswordWeakness.None, result.Weakness);
        Assert.AreEqual(PasswordStrength.Strong, result.Strength);
        Assert.IsTrue(result.EntropyBits >= PasswordStrengthPolicy.StrongEntropyBits);
    }

    [TestMethod]
    public void EstimateEntropyBits_EmptyIsZero()
    {
        Assert.AreEqual(0d, PasswordStrengthPolicy.EstimateEntropyBits(string.Empty));
        Assert.AreEqual(0d, PasswordStrengthPolicy.EstimateEntropyBits(null));
    }

    [TestMethod]
    public void GenerateStrongPassword_UsesGroupedCrockfordBase32()
    {
        var generated = PasswordStrengthPolicy.GenerateStrongPassword();

        Assert.AreEqual(39, generated.Length);
        var groups = generated.Split(PasswordStrengthPolicy.GeneratedSecretSeparator);
        Assert.AreEqual(8, groups.Length);
        foreach (var group in groups)
        {
            Assert.AreEqual(PasswordStrengthPolicy.GeneratedSecretGroupSize, group.Length);
        }

        foreach (var ch in generated)
        {
            if (ch == PasswordStrengthPolicy.GeneratedSecretSeparator)
            {
                continue;
            }

            StringAssert.Contains(PasswordStrengthPolicy.GeneratedSecretAlphabet, ch.ToString());
        }
    }

    [TestMethod]
    public void GenerateStrongPassword_PassesOwnPolicy()
    {
        var generated = PasswordStrengthPolicy.GenerateStrongPassword();
        var result = PasswordStrengthPolicy.Evaluate(generated, "alice");

        Assert.IsTrue(result.IsAcceptable);
        Assert.AreEqual(PasswordStrength.Strong, result.Strength);
    }

    [TestMethod]
    public void GenerateStrongPassword_ProducesDifferentSecrets()
    {
        var first = PasswordStrengthPolicy.GenerateStrongPassword();
        var second = PasswordStrengthPolicy.GenerateStrongPassword();

        Assert.AreNotEqual(first, second);
    }

    [TestMethod]
    public void IsExistingCredentialReuse_OnlyMatchesStoredCredential()
    {
        Assert.IsTrue(PasswordStrengthPolicy.IsExistingCredentialReuse("Alice", "shortpw", "alice", "shortpw"));
        Assert.IsFalse(PasswordStrengthPolicy.IsExistingCredentialReuse("Alice", "shortpw", "alice", "otherpw"));
        Assert.IsFalse(PasswordStrengthPolicy.IsExistingCredentialReuse("Alice", "shortpw", "bob", "shortpw"));
        Assert.IsFalse(PasswordStrengthPolicy.IsExistingCredentialReuse(null, null, "alice", "shortpw"));
        Assert.IsFalse(PasswordStrengthPolicy.IsExistingCredentialReuse("Alice", string.Empty, "alice", string.Empty));
    }
}
