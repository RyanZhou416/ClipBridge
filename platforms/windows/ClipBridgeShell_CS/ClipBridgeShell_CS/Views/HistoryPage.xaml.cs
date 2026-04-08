using System.Diagnostics;
using ClipBridgeShell_CS.Core.Models.Events;
using ClipBridgeShell_CS.Helpers;
using ClipBridgeShell_CS.ViewModels;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using WinUI3Localizer;

namespace ClipBridgeShell_CS.Views;

public sealed partial class HistoryPage : Page
{
    public HistoryViewModel ViewModel { get; }

    private ItemMetaPayload? _contextMenuItem;
    private MenuFlyout? _historyItemContextMenu;

    public HistoryPage()
    {
        ViewModel = App.GetService<HistoryViewModel>();
        InitializeComponent();
        InitHistoryItemContextMenu();
        Loaded += OnPageLoaded;
        Unloaded += OnPageUnloaded;
    }

    private void InitHistoryItemContextMenu()
    {
        _historyItemContextMenu = (MenuFlyout)Resources["HistoryItemMenu"];
        _historyItemContextMenu.Opening += HistoryItemMenu_Opening;

        var loc = WinUI3Localizer.Localizer.Get();
        var deleteLocal = loc.GetLocalizedString("Main_ContextMenu_DeleteLocal");
        if (string.IsNullOrEmpty(deleteLocal) || deleteLocal == "Main_ContextMenu_DeleteLocal") deleteLocal = "Local delete";
        var deleteGlobal = loc.GetLocalizedString("Main_ContextMenu_GlobalDelete");
        if (string.IsNullOrEmpty(deleteGlobal) || deleteGlobal == "Main_ContextMenu_GlobalDelete") deleteGlobal = "Global delete";

        var itemDelete = new MenuFlyoutItem { Text = deleteLocal };
        itemDelete.Click += HistoryItemContextMenuDeleteClick;
        _historyItemContextMenu.Items.Add(itemDelete);

        var itemGlobalDelete = new MenuFlyoutItem { Text = deleteGlobal };
        itemGlobalDelete.Click += HistoryItemContextMenuGlobalDeleteClick;
        _historyItemContextMenu.Items.Add(itemGlobalDelete);
    }

    private void HistoryItemMenu_Opening(object? sender, object e)
    {
        if (sender is MenuFlyout flyout && flyout.Target is FrameworkElement target)
            _contextMenuItem = target.DataContext as ItemMetaPayload;
    }

    private void OnPageLoaded(object sender, RoutedEventArgs e)
    {
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        UpdateAllLockIconsAndSelection();
    }

    private void OnPageUnloaded(object sender, RoutedEventArgs e)
    {
        ViewModel.PropertyChanged -= OnViewModelPropertyChanged;
    }

    private static DependencyObject? FindChildByTag(DependencyObject parent, object tag)
    {
        var count = VisualTreeHelper.GetChildrenCount(parent);
        for (var i = 0; i < count; i++)
        {
            var child = VisualTreeHelper.GetChild(parent, i);
            if (child is FrameworkElement fe && fe.Tag != null && fe.Tag.Equals(tag))
                return child;
            var found = FindChildByTag(child, tag);
            if (found != null) return found;
        }
        return null;
    }

    private void OnViewModelPropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(ViewModel.IsToastOpen))
        {
            if (ViewModel.IsToastOpen)
                ShowCopyToast();
            else
                HideCopyToast();
        }
        else if (e.PropertyName == nameof(ViewModel.LockedItemId) || e.PropertyName == nameof(ViewModel.SelectedItemId))
        {
            UpdateAllLockIconsAndSelection();
        }
    }

    private async void LockButton_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.DataContext is ItemMetaPayload item)
            await ViewModel.ToggleLockItemAsync(item);
    }

    private void UpdateAllLockIconsAndSelection()
    {
        if (HistoryList?.ItemsSource == null) return;
        var selectedBrush = GetCardSelectedBorderBrush();
        var defaultBrush = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"];
        foreach (var item in ViewModel.Source)
        {
            if (item == null) continue;
            var container = HistoryList.ContainerFromItem(item) as ListViewItem;
            if (container == null) continue;
            var btn = FindChildByTag(container, "LockButton") as Button;
            if (btn?.Content is FontIcon icon)
            {
                var isLocked = ViewModel.IsItemLocked(item.ItemId);
                icon.Glyph = isLocked ? "\uE72E" : "\uE785";
                icon.Foreground = isLocked
                    ? new SolidColorBrush(Microsoft.UI.Colors.Orange)
                    : (Brush)Application.Current.Resources["TextFillColorPrimaryBrush"];
            }
            var rowBorder = FindChildByTag(container, "HistoryRowBorder") as Border;
            if (rowBorder != null)
            {
                var isSelected = ViewModel.IsItemSelected(item.ItemId);
                rowBorder.BorderThickness = isSelected ? new Thickness(2) : new Thickness(1);
                rowBorder.BorderBrush = isSelected && selectedBrush != null ? selectedBrush : defaultBrush;
            }
        }
    }

    private Brush? GetCardSelectedBorderBrush()
    {
        var themeDicts = Resources.ThemeDictionaries;
        var currentTheme = ActualTheme == ElementTheme.Light ? "Light" : "Dark";
        if (themeDicts != null && themeDicts.TryGetValue(currentTheme, out var themeDict) && themeDict is ResourceDictionary dict
            && dict.TryGetValue("CardSelectedBorderBrush", out var resource) && resource is Brush brush)
            return brush;
        return null;
    }

    private void ShowCopyToast()
    {
        if (CopyToast == null) return;

        CopyToastIcon.Glyph = ViewModel.IsToastSuccess ? "\uE73E" : "\uEA39";
        CopyToastIcon.Foreground = ViewModel.IsToastSuccess
            ? new SolidColorBrush(Microsoft.UI.Colors.LimeGreen)
            : new SolidColorBrush(Microsoft.UI.Colors.OrangeRed);

        CopyToast.Opacity = 1;
        CopyToast.Translation = new System.Numerics.Vector3(0, 0, 32);
    }

    private void HideCopyToast()
    {
        if (CopyToast == null) return;
        CopyToast.Opacity = 0;
        CopyToast.Translation = new System.Numerics.Vector3(0, 12, 32);
    }

    private async void CopyButton_Click(object sender, RoutedEventArgs e)
    {
        var fe = sender as FrameworkElement;
        var meta = fe?.DataContext as ItemMetaPayload;
        if (meta == null) return;

        await ViewModel.CopyCommand.ExecuteAsync(meta);
    }

    private async void HistoryItemContextMenuDeleteClick(object sender, RoutedEventArgs e)
    {
        var item = _contextMenuItem;
        _contextMenuItem = null;
        if (item != null)
            await ViewModel.DeleteItemAsync(item);
    }

    private async void HistoryItemContextMenuGlobalDeleteClick(object sender, RoutedEventArgs e)
    {
        var item = _contextMenuItem;
        _contextMenuItem = null;
        if (item != null)
            await ViewModel.GlobalDeleteItemAsync(item);
    }
}
