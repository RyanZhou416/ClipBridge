using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml;
using ClipBridgeShell_CS.ViewModels;
using ClipBridgeShell_CS.Interop;
using System.Collections.Generic;
using System.Linq;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LogsCategoryFilterFlyout : UserControl
{
    public LogsViewModel? ViewModel { get; set; }
    
    public LogsCategoryFilterFlyout()
    {
        InitializeComponent();
        Loaded += (s, e) => UpdateCategoryList();
    }
    
    private bool _isRestoringState = false;
    
    private void UpdateCategoryList()
    {
        CategoryCheckBoxList.Children.Clear();
        
        if (ViewModel == null) return;
        
        // 从现有日志统计分类
        var stats = ViewModel.GetSourceStatsFromItems();
        if (stats.Categories.Count == 0) return;
        
        // 获取当前过滤条件
        var currentFilter = ViewModel.FilterCategories;
        // 如果过滤条件为空，表示全部选中
        bool allSelected = currentFilter.Count == 0;
        
        // 暂时禁用事件，避免恢复状态时触发过滤
        _isRestoringState = true;
        
        foreach (var kvp in stats.Categories.OrderByDescending(x => x.Value))
        {
            bool isChecked = allSelected || currentFilter.Contains(kvp.Key);
            var checkBox = new CheckBox
            {
                Content = $"{kvp.Key} ({kvp.Value})",
                Tag = kvp.Key,
                IsChecked = isChecked
            };
            checkBox.Checked += OnCategoryChecked;
            checkBox.Unchecked += OnCategoryUnchecked;
            CategoryCheckBoxList.Children.Add(checkBox);
        }
        
        _isRestoringState = false;
    }
    
    private void OnCategoryChecked(object sender, RoutedEventArgs e)
    {
        if (!_isRestoringState)
        {
            ApplyFilter();
        }
    }
    
    private void OnCategoryUnchecked(object sender, RoutedEventArgs e)
    {
        if (!_isRestoringState)
        {
            ApplyFilter();
        }
    }
    
    private void ApplyFilter()
    {
        if (ViewModel == null) return;
        
        var selectedCategories = new HashSet<string>();
        int totalCount = CategoryCheckBoxList.Children.Count;
        int checkedCount = 0;
        
        foreach (CheckBox cb in CategoryCheckBoxList.Children)
        {
            if (cb.IsChecked == true && cb.Tag is string category)
            {
                selectedCategories.Add(category);
                checkedCount++;
            }
        }
        
        // 如果所有项都选中，设置为空集合（表示不过滤）
        if (checkedCount == totalCount)
        {
            ViewModel.FilterCategories = new HashSet<string>();
        }
        else
        {
            ViewModel.FilterCategories = selectedCategories;
        }
    }
    
    private void OnResetClick(object sender, RoutedEventArgs e)
    {
        foreach (CheckBox cb in CategoryCheckBoxList.Children)
        {
            cb.IsChecked = true;
        }
        if (ViewModel != null)
        {
            ViewModel.FilterCategories = new HashSet<string>();
        }
    }
}
