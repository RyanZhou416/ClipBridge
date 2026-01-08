using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml;
using ClipBridgeShell_CS.ViewModels;

namespace ClipBridgeShell_CS.Views;

public sealed partial class LogsTimestampFilterFlyout : UserControl
{
    public LogsViewModel? ViewModel { get; set; }
    
    private bool _isRestoringState = false;
    
    public LogsTimestampFilterFlyout()
    {
        InitializeComponent();
        Loaded += (s, e) => { RestoreFilterState(); AttachEvents(); };
    }
    
    private void RestoreFilterState()
    {
        if (ViewModel == null) return;
        
        // 暂时禁用事件，避免恢复状态时触发过滤
        _isRestoringState = true;
        
        // 恢复开始时间过滤
        if (ViewModel.FilterStartTime.HasValue)
        {
            StartTimeUnlimitedCheckBox.IsChecked = false;
            StartDatePicker.Date = ViewModel.FilterStartTime.Value.Date;
            StartTimePicker.Time = ViewModel.FilterStartTime.Value.TimeOfDay;
        }
        else
        {
            StartTimeUnlimitedCheckBox.IsChecked = true;
        }
        
        // 恢复结束时间过滤
        if (ViewModel.FilterEndTime.HasValue)
        {
            EndTimeNowCheckBox.IsChecked = false;
            EndDatePicker.Date = ViewModel.FilterEndTime.Value.Date;
            EndTimePicker.Time = ViewModel.FilterEndTime.Value.TimeOfDay;
        }
        else
        {
            EndTimeNowCheckBox.IsChecked = true;
        }
        
        _isRestoringState = false;
    }
    
    private void AttachEvents()
    {
        StartTimeUnlimitedCheckBox.Checked += OnTimeFilterChanged;
        StartTimeUnlimitedCheckBox.Unchecked += OnTimeFilterChanged;
        EndTimeNowCheckBox.Checked += OnTimeFilterChanged;
        EndTimeNowCheckBox.Unchecked += OnTimeFilterChanged;
        StartDatePicker.DateChanged += (s, e) => ApplyTimeFilter();
        StartTimePicker.TimeChanged += (s, e) => ApplyTimeFilter();
        EndDatePicker.DateChanged += (s, e) => ApplyTimeFilter();
        EndTimePicker.TimeChanged += (s, e) => ApplyTimeFilter();
    }
    
    private void OnTimeFilterChanged(object sender, RoutedEventArgs e)
    {
        if (!_isRestoringState)
        {
            ApplyTimeFilter();
        }
    }
    
    private void ApplyTimeFilter()
    {
        if (ViewModel == null || _isRestoringState) return;
        
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
        
        ViewModel.FilterStartTime = startTime;
        ViewModel.FilterEndTime = endTime;
    }
    
    private void OnResetClick(object sender, RoutedEventArgs e)
    {
        StartTimeUnlimitedCheckBox.IsChecked = true;
        EndTimeNowCheckBox.IsChecked = true;
        if (ViewModel != null)
        {
            ViewModel.FilterStartTime = null;
            ViewModel.FilterEndTime = null;
        }
    }
}
