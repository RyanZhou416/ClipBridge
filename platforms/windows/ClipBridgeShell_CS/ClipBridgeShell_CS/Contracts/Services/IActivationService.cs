namespace ClipBridgeShell_CS.Contracts.Services;

public interface IActivationService
{
    Task ActivateAsync(object activationArgs);
}
