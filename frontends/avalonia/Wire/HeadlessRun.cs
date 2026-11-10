using System.Text;
using Gongga.Avalonia.ViewModels;

namespace Gongga.Avalonia.Wire;

/// Runs the same transport and frame model without a window, so a real client
/// child can be verified on a machine with no interactive desktop.
public static class HeadlessRun
{
    public static async Task<int> RunAsync(ClientOptions options)
    {
        Console.OutputEncoding = Encoding.UTF8;
        using var client = new ClientProcess();
        var model = new ClientModel { Send = client.Send };
        var types = new List<string>();
        var laidOut = new TaskCompletionSource<bool>(
            TaskCreationOptions.RunContinuationsAsynchronously);

        var error = client.Start(options, frame =>
        {
            types.Add(frame.GetType().Name);
            model.Apply(frame);
            if (frame is RenderFrame.Layout)
            {
                laidOut.TrySetResult(true);
            }
        });
        if (error is not null)
        {
            Console.WriteLine($"FAIL \u00b7 {error}");
            return 1;
        }

        var finished = await Task.WhenAny(laidOut.Task, client.Exited).ConfigureAwait(false);
        var reachedLayout = finished == laidOut.Task;
        if (reachedLayout)
        {
            client.Send(Intents.Quit());
            await client.Exited.ConfigureAwait(false);
        }
        client.Stop();

        Console.WriteLine(
            $"{(reachedLayout ? "PASS" : "FAIL")} \u00b7 frames={types.Count} \u00b7 " +
            $"types={string.Join(",", types)} \u00b7 status={model.StatusText} \u00b7 " +
            $"sessions={model.Sessions.Count} \u00b7 panes={model.Panes.Count}");
        return reachedLayout ? 0 : 1;
    }
}
