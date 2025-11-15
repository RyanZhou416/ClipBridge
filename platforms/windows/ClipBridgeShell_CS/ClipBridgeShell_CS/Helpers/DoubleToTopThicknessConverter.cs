using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;


namespace ClipBridgeShell_CS.Helpers
{
    public sealed class DoubleToTopThicknessConverter : IValueConverter
    {
        public object Convert(object value, System.Type targetType, object parameter, string language)
        {
            double h = value is double d ? d : 0;
            return new Thickness(0, h, 0, 0);
        }
        public object ConvertBack(object value, System.Type targetType, object parameter, string language) => 0d;
    }
}
