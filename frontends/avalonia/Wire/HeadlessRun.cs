using System.Text;
using Gongga.Avalonia.ViewModels;

namespace Gongga.Avalonia.Wire;

/// Runs the same transport and frame model without a window, so a real client
/// child can be verified on a machine with no interactive desktop.
public static class HeadlessRun
{
    /// How long a greeting is given. A local dial is milliseconds; a remote one
    /// that never answers must still end with a verdict rather than hang.
    private static readonly TimeSpan Greeting = TimeSpan.FromSeconds(6);

    /// How long the child is given to leave after `quit`.
    private static readonly TimeSpan Ending = TimeSpan.FromSeconds(3);

    public static async Task<int> RunAsync(ClientOptions options)
    {
        Console.OutputEncoding = Encoding.UTF8;
        using var client = new ClientProcess();
        var model = new ClientModel { Send = client.Send };
        var types = new List<string>();
        var connected = new TaskCompletionSource<bool>(
            TaskCreationOptions.RunContinuationsAsynchronously);

        var error = client.Start(options, frame =>
        {
            types.Add(frame.GetType().Name);
            model.Apply(frame);
            // Contract: the layout that precedes a greeting is the offline picture,
            // so readiness is a `Server` frame with a label, not a frame count.
            if (frame is RenderFrame.Server { Label: not null })
            {
                connected.TrySetResult(true);
            }
        });
        if (error is not null)
        {
            Console.WriteLine($"FAIL \u00b7 {error}");
            return 1;
        }

        var finished = await Task.WhenAny(connected.Task, client.Exited, Task.Delay(Greeting))
            .ConfigureAwait(false);
        var isConnected = finished == connected.Task;

        client.Send(Intents.Quit());
        await Task.WhenAny(client.Exited, Task.Delay(Ending)).ConfigureAwait(false);
        client.Stop();

        var piped = types.Count != 0;
        Console.WriteLine(
            $"{(piped ? "PASS" : "FAIL")} \u00b7 connected={isConnected} \u00b7 " +
            $"frames={types.Count} \u00b7 types={string.Join(",", types)} \u00b7 " +
            $"status={model.StatusText} \u00b7 sessions={model.Sessions.Count} \u00b7 " +
            $"panes={model.Panes.Count}");
        return piped ? 0 : 1;
    }
}
