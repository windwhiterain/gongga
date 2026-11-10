namespace Gongga.Avalonia.Wire;

public sealed record LaunchOptions(ClientOptions Client, bool NoWindow);

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
        string? key = null;
        var clientOnly = false;
        var force = false;
        var noWindow = false;

        for (var index = 0; index < args.Length; index++)
        {
            var token = args[index];
            switch (token)
            {
                case "--client":
                case "--server":
                case "--home":
                case "--client-home":
                case "--key":
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
                        case "--key": key = value; break;
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

        options = new LaunchOptions(
            new ClientOptions(executable, server, home, clientHome, key, clientOnly, force),
            noWindow);
        return null;

        static LaunchOptions Empty() => new(
            new ClientOptions(null, null, null, null, null, false, false), false);
    }
}
