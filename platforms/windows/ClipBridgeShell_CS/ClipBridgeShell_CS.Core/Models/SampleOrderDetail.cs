namespace ClipBridgeShell_CS.Core.Models;

// Model for the SampleDataService. Replace with your own model.
public class SampleOrderDetail
{
    public long ProductID
    {
        get; set;
    }

    public string ProductName
    {
        get; set;
    } = string.Empty;

    public int Quantity
    {
        get; set;
    }

    public double Discount
    {
        get; set;
    }

    public string QuantityPerUnit
    {
        get; set;
    } = string.Empty;

    public double UnitPrice
    {
        get; set;
    }

    public string CategoryName
    {
        get; set;
    } = string.Empty;

    public string CategoryDescription
    {
        get; set;
    } = string.Empty;

    public double Total
    {
        get; set;
    }

    public string ShortDescription => $"Product ID: {ProductID} - {ProductName}";
}
