using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml;
using ClipBridgeShell_CS.ViewModels;
using System.Collections.Generic;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LogsLevelFilterFlyout : UserControl
{
    public LogsViewModel? ViewModel { get; set; }
    
    private bool _isRestoringState = false;
    
    public LogsLevelFilterFlyout()
    {
        InitializeComponent();
        Loaded += (s, e) => { RestoreFilterState(); AttachLevelEvents(); };
    }
    
    private void RestoreFilterState()
    {
        if (ViewModel == null) return;
        
        var currentFilter = ViewModel.FilterLevels;
        // 如果过滤条件为空或包含所有级别，表示全部选中
        bool allSelected = currentFilter.Count == 0 || currentFilter.Count == 6;
        
        // 暂时禁用事件，避免恢复状态时触发过滤
        _isRestoringState = true;
        
        if (allSelected)
        {
            LevelTraceCheckBox.IsChecked = true;
            LevelDebugCheckBox.IsChecked = true;
            LevelInfoCheckBox.IsChecked = true;
            LevelWarnCheckBox.IsChecked = true;
            LevelErrorCheckBox.IsChecked = true;
            LevelCriticalCheckBox.IsChecked = true;
        }
        else
        {
            LevelTraceCheckBox.IsChecked = currentFilter.Contains(0);
            LevelDebugCheckBox.IsChecked = currentFilter.Contains(1);
            LevelInfoCheckBox.IsChecked = currentFilter.Contains(2);
            LevelWarnCheckBox.IsChecked = currentFilter.Contains(3);
            LevelErrorCheckBox.IsChecked = currentFilter.Contains(4);
            LevelCriticalCheckBox.IsChecked = currentFilter.Contains(5);
        }
        
        _isRestoringState = false;
    }
    
    private void AttachLevelEvents()
    {
        LevelTraceCheckBox.Checked += OnLevelChecked;
        LevelTraceCheckBox.Unchecked += OnLevelUnchecked;
        LevelDebugCheckBox.Checked += OnLevelChecked;
        LevelDebugCheckBox.Unchecked += OnLevelUnchecked;
        LevelInfoCheckBox.Checked += OnLevelChecked;
        LevelInfoCheckBox.Unchecked += OnLevelUnchecked;
        LevelWarnCheckBox.Checked += OnLevelChecked;
        LevelWarnCheckBox.Unchecked += OnLevelUnchecked;
        LevelErrorCheckBox.Checked += OnLevelChecked;
        LevelErrorCheckBox.Unchecked += OnLevelUnchecked;
        LevelCriticalCheckBox.Checked += OnLevelChecked;
        LevelCriticalCheckBox.Unchecked += OnLevelUnchecked;
    }
    
    private void OnLevelChecked(object sender, RoutedEventArgs e)
    {
        if (!_isRestoringState)
        {
            ApplyFilter();
        }
    }
    
    private void OnLevelUnchecked(object sender, RoutedEventArgs e)
    {
        if (!_isRestoringState)
        {
            ApplyFilter();
        }
    }
    
    private void ApplyFilter()
    {
        if (ViewModel == null) return;
        
        var selectedLevels = new HashSet<int>();
        if (LevelTraceCheckBox.IsChecked == true) selectedLevels.Add(0);
        if (LevelDebugCheckBox.IsChecked == true) selectedLevels.Add(1);
        if (LevelInfoCheckBox.IsChecked == true) selectedLevels.Add(2);
        if (LevelWarnCheckBox.IsChecked == true) selectedLevels.Add(3);
        if (LevelErrorCheckBox.IsChecked == true) selectedLevels.Add(4);
        if (LevelCriticalCheckBox.IsChecked == true) selectedLevels.Add(5);
        
        ViewModel.FilterLevels = selectedLevels;
    }
    
    private void OnResetClick(object sender, RoutedEventArgs e)
    {
        LevelTraceCheckBox.IsChecked = true;
        LevelDebugCheckBox.IsChecked = true;
        LevelInfoCheckBox.IsChecked = true;
        LevelWarnCheckBox.IsChecked = true;
        LevelErrorCheckBox.IsChecked = true;
        LevelCriticalCheckBox.IsChecked = true;
        if (ViewModel != null)
        {
            ViewModel.FilterLevels = new HashSet<int> { 0, 1, 2, 3, 4, 5 };
        }
    }
}
