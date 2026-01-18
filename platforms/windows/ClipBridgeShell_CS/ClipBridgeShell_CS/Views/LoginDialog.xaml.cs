using ClipBridgeShell_CS.Contracts.Services;
using ClipBridgeShell_CS.Core.Contracts.Services;
using ClipBridgeShell_CS.Services;
using Microsoft.UI.Xaml.Controls;
using WinUI3Localizer;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LoginDialog : ContentDialog
{
    private readonly IAccountService _accountService;

    public LoginDialog(IAccountService accountService)
    {
        _accountService = accountService;
        InitializeComponent();
    }

    private void OnInputChanged(object sender, object e)
    {
        // 隐藏错误信息
        ErrorTextBlock.Visibility = Microsoft.UI.Xaml.Visibility.Collapsed;
        
        // 更新登录按钮状态
        IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(UsernameTextBox.Text) 
                                 && !string.IsNullOrWhiteSpace(PasswordBox.Password);
    }

    private async void OnPrimaryButtonClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        // 延迟关闭，等待验证
        var deferral = args.GetDeferral();

        try
        {
            var username = UsernameTextBox.Text?.Trim();
            var password = PasswordBox.Password;

            // 验证输入
            if (string.IsNullOrWhiteSpace(username) || string.IsNullOrWhiteSpace(password))
            {
                var loc = Localizer.Get();
                ErrorTextBlock.Text = loc.GetLocalizedString("LoginDialog_ErrorEmpty");
                ErrorTextBlock.Visibility = Microsoft.UI.Xaml.Visibility.Visible;
                args.Cancel = true;
                return;
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
            ErrorTextBlock.Text = string.Format(loc.GetLocalizedString("LoginDialog_ErrorFailed"), ex.Message);
            ErrorTextBlock.Visibility = Microsoft.UI.Xaml.Visibility.Visible;
            args.Cancel = true;
        }
        finally
        {
            deferral.Complete();
        }
    }
}
