namespace Gongga.Avalonia.Wire;

public sealed record LaunchOptions(ClientOptions Client, bool NoWindow, string? Shots);

public static class LaunchParser
{
    /// Returns null on success; a bad argv is reported to the operator instead of
    /// ending the process with an unhandled exception.
    public static string? Parse(string[] args, out LaunchOptions options)
    {
        string? executable = null;
        string? server = null;
        string? home = null;
        string? clientHome = null;
        string? instance = null;
        string? key = null;
        var clientOnly = false;
        var force = false;
        var noWindow = false;
        string? shots = null;

        for (var index = 0; index < args.Length; index++)
        {
            var token = args[index];
            switch (token)
            {
                case "--client":
                case "--server":
                case "--home":
                case "--client-home":
                case "--instance":
                case "--key":
                case "--shots":
                    if (index + 1 == args.Length)
                    {
                        options = Empty();
                        return $"{token} needs a value";
                    }
                    var value = args[++index];
                    switch (token)
                    {
                        case "--client": executable = value; break;
                        case "--server": server = value; break;
                        case "--home": home = value; break;
                        case "--client-home": clientHome = value; break;
                        case "--instance": instance = value; break;
                        case "--key": key = value; break;
                        case "--shots": shots = value; break;
                    }
                    break;
                case "--client_only":
                case "--client-only":
                    clientOnly = true;
                    break;
                case "--force":
                    force = true;
                    break;
                case "--no-window":
                    noWindow = true;
                    break;
                default:
                    options = Empty();
                    return $"unknown option: {token}";
            }
        }

        // One client per client home (the client takes a lock), so a second window
        // names its own: `<client-home>/<instance>`, defaulting the base directory
        // to the same place the client itself would.
        if (instance is not null)
        {
            if (instance.Length == 0
                || instance != Path.GetFileName(instance)
                || instance is "." or "..")
            {
                options = Empty();
                return "--instance needs a plain name, not a path";
            }
            var baseDirectory = clientHome ?? Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
                ".gongga-client");
            clientHome = Path.Combine(baseDirectory, instance);
        }

        options = new LaunchOptions(
            new ClientOptions(executable, server, home, clientHome, key, clientOnly, force),
            noWindow,
            shots);
        return null;

        static LaunchOptions Empty() => new(
            new ClientOptions(null, null, null, null, null, false, false), false, null);
    }
}
