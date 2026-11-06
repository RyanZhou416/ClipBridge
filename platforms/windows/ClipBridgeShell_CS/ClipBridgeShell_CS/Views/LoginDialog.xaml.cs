using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Contracts.Services;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;
using WinUI3Localizer;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LoginDialog : ContentDialog
{
    private static readonly Brush s_weakBrush = new SolidColorBrush(Microsoft.UI.Colors.OrangeRed);
    private static readonly Brush s_okBrush = new SolidColorBrush(Microsoft.UI.Colors.Orange);
    private static readonly Brush s_strongBrush = new SolidColorBrush(Microsoft.UI.Colors.Green);

    private readonly IAccountService _accountService;

    public LoginDialog(IAccountService accountService)
    {
        _accountService = accountService;
        InitializeComponent();
    }

    private void OnInputChanged(object sender, object e)
    {
        // 隐藏错误信息
        ErrorTextBlock.Visibility = Visibility.Collapsed;

        // 用户改动了密码后，之前生成的只读展示已不再对应当前密码，收起避免复制到旧值。
        if (GeneratedPasswordPanel.Visibility == Visibility.Visible
            && !string.Equals(PasswordBox.Password, GeneratedPasswordBox.Text, StringComparison.Ordinal))
        {
            GeneratedPasswordPanel.Visibility = Visibility.Collapsed;
            GeneratedPasswordBox.Text = string.Empty;
        }

        // 更新登录按钮状态
        IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(UsernameTextBox.Text)
                                 && !string.IsNullOrWhiteSpace(PasswordBox.Password);

        UpdatePasswordHint();
    }

    private async void OnPrimaryButtonClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        // 延迟关闭，等待验证
        var deferral = args.GetDeferral();

        try
        {
            var loc = Localizer.Get();
            var username = UsernameTextBox.Text?.Trim();
            var password = PasswordBox.Password;

            // 验证输入
            if (string.IsNullOrWhiteSpace(username) || string.IsNullOrWhiteSpace(password))
            {
                ShowError(loc.GetLocalizedString("LoginDialog_ErrorEmpty"));
                args.Cancel = true;
                return;
            }

            // 强度门槛只作用于“创建账号/修改密码”。若本地已有完全相同的凭据，
            // 说明这是重复登录（可能是历史短密码），不阻止，保持老用户可用。
            var existing = await _accountService.LoadAccountStateAsync();
            var isExistingCredential = existing.PasswordAvailable
                && PasswordStrengthPolicy.IsExistingCredentialReuse(
                    existing.Username,
                    existing.Password,
                    username,
                    password);

            if (!isExistingCredential)
            {
                var evaluation = PasswordStrengthPolicy.Evaluate(password, username);
                if (!evaluation.IsAcceptable)
                {
                    ShowError(GetWeaknessMessage(loc, evaluation.Weakness));
                    args.Cancel = true;
                    return;
                }
            }

            // 保存账号信息
            await _accountService.SaveAccountAsync(username, password);

            // 触发核心初始化
            var coreHost = App.GetService<ICoreHostService>();
            if (coreHost is CoreHostService coreHostService)
            {
                _ = coreHostService.InitializeWithAccountAsync();
            }
        }
        catch (Exception ex)
        {
            var loc = Localizer.Get();
            ShowError(string.Format(loc.GetLocalizedString("LoginDialog_ErrorFailed"), ex.Message));
            args.Cancel = true;
        }
        finally
        {
            deferral.Complete();
        }
    }

    private void OnGeneratePasswordClick(object sender, RoutedEventArgs e)
    {
        var generated = PasswordStrengthPolicy.GenerateStrongPassword();

        // 同时填进密码框并显示在只读字段里：后者让用户能看清/复制并保存到密码管理器。
        PasswordBox.Password = generated;
        GeneratedPasswordBox.Text = generated;
        GeneratedPasswordPanel.Visibility = Visibility.Visible;
        CopyGeneratedPasswordButton.Content = Localizer.Get().GetLocalizedString("LoginDialog_CopyPasswordButton");

        ErrorTextBlock.Visibility = Visibility.Collapsed;
        UpdatePasswordHint();
    }

    private async void OnCopyGeneratedPasswordClick(object sender, RoutedEventArgs e)
    {
        if (string.IsNullOrEmpty(GeneratedPasswordBox.Text))
        {
            return;
        }

        var loc = Localizer.Get();
        try
        {
            var package = new DataPackage();
            package.SetText(GeneratedPasswordBox.Text);
            Clipboard.SetContent(package);

            CopyGeneratedPasswordButton.Content = loc.GetLocalizedString("LoginDialog_Copied");
            await Task.Delay(1500);
            CopyGeneratedPasswordButton.Content = loc.GetLocalizedString("LoginDialog_CopyPasswordButton");
        }
        catch (Exception ex)
        {
            ShowError(string.Format(loc.GetLocalizedString("LoginDialog_ErrorCopyFailed"), ex.Message));
        }
    }

    private void UpdatePasswordHint()
    {
        var password = PasswordBox.Password;
        if (string.IsNullOrEmpty(password))
        {
            PasswordHintTextBlock.Visibility = Visibility.Collapsed;
            return;
        }

        var loc = Localizer.Get();
        var result = PasswordStrengthPolicy.Evaluate(password, UsernameTextBox.Text?.Trim());

        string text;
        Brush brush;
        switch (result.Strength)
        {
            case PasswordStrength.Strong:
                text = loc.GetLocalizedString("LoginDialog_PasswordHintStrong");
                brush = s_strongBrush;
                break;
            case PasswordStrength.Ok:
                text = loc.GetLocalizedString("LoginDialog_PasswordHintOk");
                brush = s_okBrush;
                break;
            default:
                text = loc.GetLocalizedString("LoginDialog_PasswordHintWeak");
                brush = s_weakBrush;
                break;
        }

        PasswordHintTextBlock.Text = text;
        PasswordHintTextBlock.Foreground = brush;
        PasswordHintTextBlock.Visibility = Visibility.Visible;
    }

    private void ShowError(string message)
    {
        ErrorTextBlock.Text = message;
        ErrorTextBlock.Visibility = Visibility.Visible;
    }

    private static string GetWeaknessMessage(ILocalizer loc, PasswordWeakness weakness)
    {
        return weakness switch
        {
            PasswordWeakness.TooShort => loc.GetLocalizedString("LoginDialog_ErrorPasswordTooShort"),
            PasswordWeakness.SameAsUsername => loc.GetLocalizedString("LoginDialog_ErrorPasswordSameAsUsername"),
            PasswordWeakness.CommonPattern => loc.GetLocalizedString("LoginDialog_ErrorPasswordCommonPattern"),
            PasswordWeakness.LowEntropy => loc.GetLocalizedString("LoginDialog_ErrorPasswordLowEntropy"),
            _ => loc.GetLocalizedString("LoginDialog_ErrorPasswordTooShort"),
        };
    }
}
