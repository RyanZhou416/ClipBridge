using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml;
using System;
using System.Collections.Generic;
using System.Linq;
using ClipBridgeShell_CS.Interop;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LogsFilterFlyout : UserControl
{
    public event EventHandler<FilterAppliedEventArgs>? FilterApplied;
    
    public SourceStats? SourceStats
    {
        get => _sourceStats;
        set
        {
            _sourceStats = value;
            LoadSourceStats(value);
        }
    }
    
    private SourceStats? _sourceStats;
    
    public LogsFilterFlyout()
    {
        InitializeComponent();
    }
    
    public void LoadSourceStats(SourceStats? stats)
    {
        _sourceStats = stats;
        UpdateComponentList();
        UpdateCategoryList();
    }
    
    private void UpdateComponentList()
    {
        ComponentCheckBoxList.Children.Clear();
        
        if (_sourceStats?.Components == null) return;
        
        foreach (var kvp in _sourceStats.Components.OrderByDescending(x => x.Value))
        {
            var checkBox = new CheckBox
            {
                Content = $"{kvp.Key} ({kvp.Value})",
                Tag = kvp.Key,
                IsChecked = true
            };
            ComponentCheckBoxList.Children.Add(checkBox);
        }
    }
    
    private void UpdateCategoryList()
    {
        CategoryCheckBoxList.Children.Clear();
        
        if (_sourceStats?.Categories == null) return;
        
        foreach (var kvp in _sourceStats.Categories.OrderByDescending(x => x.Value))
        {
            var checkBox = new CheckBox
            {
                Content = $"{kvp.Key} ({kvp.Value})",
                Tag = kvp.Key,
                IsChecked = true
            };
            CategoryCheckBoxList.Children.Add(checkBox);
        }
    }
    
    private void OnResetClick(object sender, RoutedEventArgs e)
    {
        // 重置所有过滤条件
        StartTimeUnlimitedCheckBox.IsChecked = true;
        EndTimeNowCheckBox.IsChecked = true;
        
        LevelTraceCheckBox.IsChecked = true;
        LevelDebugCheckBox.IsChecked = true;
        LevelInfoCheckBox.IsChecked = true;
        LevelWarnCheckBox.IsChecked = true;
        LevelErrorCheckBox.IsChecked = true;
        LevelCriticalCheckBox.IsChecked = true;
        
        foreach (CheckBox cb in ComponentCheckBoxList.Children)
        {
            cb.IsChecked = true;
        }
        
        foreach (CheckBox cb in CategoryCheckBoxList.Children)
        {
            cb.IsChecked = true;
        }
    }
    
    private void OnApplyClick(object sender, RoutedEventArgs e)
    {
        DateTimeOffset? startTime = null;
        if (StartTimeUnlimitedCheckBox.IsChecked == false)
        {
            DateTimeOffset? startDate = StartDatePicker.Date;
            if (startDate != null)
            {
                startTime = new DateTimeOffset(startDate.Value.DateTime + StartTimePicker.Time);
            }
        }
        
        DateTimeOffset? endTime = null;
        if (EndTimeNowCheckBox.IsChecked == false)
        {
            DateTimeOffset? endDate = EndDatePicker.Date;
            if (endDate != null)
            {
                endTime = new DateTimeOffset(endDate.Value.DateTime + EndTimePicker.Time);
            }
        }
        
        var args = new FilterAppliedEventArgs
        {
            StartTimeUnlimited = StartTimeUnlimitedCheckBox.IsChecked == true,
            StartTime = startTime,
            EndTimeNow = EndTimeNowCheckBox.IsChecked == true,
            EndTime = endTime,
            SelectedLevels = new List<int>(),
            SelectedComponents = new List<string>(),
            SelectedCategories = new List<string>()
        };
        
        // 收集选中的级别
        if (LevelTraceCheckBox.IsChecked == true) args.SelectedLevels.Add(0);
        if (LevelDebugCheckBox.IsChecked == true) args.SelectedLevels.Add(1);
        if (LevelInfoCheckBox.IsChecked == true) args.SelectedLevels.Add(2);
        if (LevelWarnCheckBox.IsChecked == true) args.SelectedLevels.Add(3);
        if (LevelErrorCheckBox.IsChecked == true) args.SelectedLevels.Add(4);
        if (LevelCriticalCheckBox.IsChecked == true) args.SelectedLevels.Add(5);
        
        // 收集选中的组件
        foreach (CheckBox cb in ComponentCheckBoxList.Children)
        {
            if (cb.IsChecked == true && cb.Tag is string component)
            {
                args.SelectedComponents.Add(component);
            }
        }
        
        // 收集选中的分类
        foreach (CheckBox cb in CategoryCheckBoxList.Children)
        {
            if (cb.IsChecked == true && cb.Tag is string category)
            {
                args.SelectedCategories.Add(category);
            }
        }
        
        FilterApplied?.Invoke(this, args);
    }
}

public class FilterAppliedEventArgs : EventArgs
{
    public bool StartTimeUnlimited { get; set; }
    public DateTimeOffset? StartTime { get; set; }
    public bool EndTimeNow { get; set; }
    public DateTimeOffset? EndTime { get; set; }
    public List<int> SelectedLevels { get; set; } = new();
    public List<string> SelectedComponents { get; set; } = new();
    public List<string> SelectedCategories { get; set; } = new();
}
