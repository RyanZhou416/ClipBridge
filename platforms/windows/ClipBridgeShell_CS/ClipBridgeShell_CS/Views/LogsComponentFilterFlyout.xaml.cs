using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml;
using ClipBridgeShell_CS.ViewModels;
using ClipBridgeShell_CS.Interop;
using System.Collections.Generic;
using System.Linq;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LogsComponentFilterFlyout : UserControl
{
    public LogsViewModel? ViewModel { get; set; }
    
    public LogsComponentFilterFlyout()
    {
        InitializeComponent();
        Loaded += (s, e) => UpdateComponentList();
    }
    
    private bool _isRestoringState = false;
    
    private void UpdateComponentList()
    {
        ComponentCheckBoxList.Children.Clear();
        
        if (ViewModel == null) return;
        
        // 从现有日志统计组件
        var stats = ViewModel.GetSourceStatsFromItems();
        if (stats.Components.Count == 0) return;
        
        // 获取当前过滤条件
        var currentFilter = ViewModel.FilterComponents;
        // 如果过滤条件为空，表示全部选中
        bool allSelected = currentFilter.Count == 0;
        
        // 暂时禁用事件，避免恢复状态时触发过滤
        _isRestoringState = true;
        
        foreach (var kvp in stats.Components.OrderByDescending(x => x.Value))
        {
            bool isChecked = allSelected || currentFilter.Contains(kvp.Key);
            var checkBox = new CheckBox
            {
                Content = $"{kvp.Key} ({kvp.Value})",
                Tag = kvp.Key,
                IsChecked = isChecked
            };
            checkBox.Checked += OnComponentChecked;
            checkBox.Unchecked += OnComponentUnchecked;
            ComponentCheckBoxList.Children.Add(checkBox);
        }
        
        _isRestoringState = false;
    }
    
    private void OnComponentChecked(object sender, RoutedEventArgs e)
    {
        if (!_isRestoringState)
        {
            ApplyFilter();
        }
    }
    
    private void OnComponentUnchecked(object sender, RoutedEventArgs e)
    {
        if (!_isRestoringState)
        {
            ApplyFilter();
        }
    }
    
    private void ApplyFilter()
    {
        if (ViewModel == null) return;
        
        var selectedComponents = new HashSet<string>();
        int totalCount = ComponentCheckBoxList.Children.Count;
        int checkedCount = 0;
        
        foreach (CheckBox cb in ComponentCheckBoxList.Children)
        {
            if (cb.IsChecked == true && cb.Tag is string component)
            {
                selectedComponents.Add(component);
                checkedCount++;
            }
        }
        
        // 如果所有项都选中，设置为空集合（表示不过滤）
        if (checkedCount == totalCount)
        {
            ViewModel.FilterComponents = new HashSet<string>();
        }
        else
        {
            ViewModel.FilterComponents = selectedComponents;
        }
    }
    
    private void OnResetClick(object sender, RoutedEventArgs e)
    {
        foreach (CheckBox cb in ComponentCheckBoxList.Children)
        {
            cb.IsChecked = true;
        }
        if (ViewModel != null)
        {
            ViewModel.FilterComponents = new HashSet<string>();
        }
    }
}
