// The C# binding against the real library, the way an engine uses it:
//   dotnet Aegis.Check.dll [layout] [noise]
// `layout` prints every struct's size and field offsets and every constant
// under its C name, which crates/ffi/tests/csharp.rs compares with Rust's.
// `noise` also runs the Noise handshake (library built with feature noise).
// Exits 0 and prints "ok" when every check holds.

using System;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using Aegis;

static class Program
{
    static int failures;

    static void Expect(bool ok, string what)
    {
        if (ok) return;
        failures++;
        Console.Error.WriteLine("FAILED: " + what);
    }

    static Status Throws(Action a)
    {
        try
        {
            a();
        }
        catch (AegisException e)
        {
            return e.Status;
        }
        return Status.Ok;
    }

    static int Main(string[] args)
    {
        if (args.Contains("layout")) Layout();
        Expect(AegisMonitor.LibraryAbiVersion == AegisMonitor.AbiVersion, "abi version");
        Monitor();
        Engine();
        Client();
        Reentry();
        if (args.Contains("noise")) Noise();
        if (failures != 0) return 1;
        Console.WriteLine("ok");
        return 0;
    }

    // ---- layout ----

    // AimErr -> aim_err; LiveByte -> live (the byte behind a C bool).
    static string Snake(string name)
    {
        if (name.EndsWith("Byte")) name = name.Substring(0, name.Length - 4);
        var sb = new StringBuilder();
        for (int i = 0; i < name.Length; i++)
        {
            if (char.IsUpper(name[i]) && i > 0) sb.Append('_');
            sb.Append(char.ToLowerInvariant(name[i]));
        }
        return sb.ToString();
    }

    static void Struct<T>(string cName)
    {
        Console.WriteLine($"size {cName} {Marshal.SizeOf<T>()}");
        var fields = typeof(T).GetFields(System.Reflection.BindingFlags.Public | System.Reflection.BindingFlags.Instance)
            .Select(f => (name: Snake(f.Name), off: (long)Marshal.OffsetOf<T>(f.Name)))
            .OrderBy(f => f.off);
        foreach (var f in fields) Console.WriteLine($"offset {cName}.{f.name} {f.off}");
    }

    static void Const(string name, long value) => Console.WriteLine($"const {name} {value}");

    static void Layout()
    {
        Struct<Config>("AegisConfig");
        Struct<Alert>("AegisAlert");
        Struct<Stats>("AegisStats");
        Struct<Received>("AegisReceived");
        Struct<Player>("AegisPlayer");
        Struct<ShotEvidence>("AegisShotEvidence");
        Const("AEGIS_ABI_VERSION", AegisMonitor.AbiVersion);
        foreach (Status s in Enum.GetValues(typeof(Status)))
            Const(s == Status.Ok ? "AEGIS_OK" : "AEGIS_ERR_" + Snake(s.ToString()).ToUpperInvariant(), (int)s);
        foreach (Reason r in Enum.GetValues(typeof(Reason)))
            Const("AEGIS_REASON_" + Snake(r.ToString()).ToUpperInvariant(), (byte)r);
        Const("AEGIS_CLIENT_KEYS_LEN", AegisClient.KeysLen);
        Const("AEGIS_NAME_MAX", AegisClient.NameMax);
        Const("AEGIS_SEND_MAX", AegisClient.SendMax);
        Const("AEGIS_TICK_NEWEST", AegisClient.TickNewest);
        Const("AEGIS_RX_JOINED", Rx.Joined);
        Const("AEGIS_RX_CHALLENGE", Rx.Challenge);
        Const("AEGIS_RX_SNAPSHOT", Rx.Snapshot);
        Const("AEGIS_RX_EVENT", Rx.Event);
        Const("AEGIS_EVENT_HIT", Event.Hit);
        Const("AEGIS_EVENT_DEATH", Event.Death);
        Const("AEGIS_EVENT_JOIN", Event.Join);
        Const("AEGIS_EVENT_LEAVE", Event.Leave);
        Const("AEGIS_NOISE_PUBLIC_LEN", NoiseHandshake.PublicLen);
        Const("AEGIS_NOISE_HELLO_LEN", NoiseHandshake.HelloLen);
        Const("AEGIS_NOISE_CHALLENGE", (int)NoiseAnswer.Challenge);
        Const("AEGIS_NOISE_WELCOME", (int)NoiseAnswer.Welcome);
    }

    // ---- monitor: config, bools, statuses ----

    static void Monitor()
    {
        var cfg = Config.AtTickRate(60);
        Expect(cfg.Validate() == null, "the 60 Hz config is valid");
        Expect(cfg.ReactionFastTicks > Config.AtTickRate(30).ReactionFastTicks, "ticks scale with the rate");
        Expect(Throws(() => Config.AtTickRate(0)) == Status.Arg, "0 Hz is refused");
        var bad = cfg;
        bad.ForesightClearRad = bad.ForesightFitRad;
        string why = bad.Validate();
        Expect(why != null && why.StartsWith("foresight.clear_rad"), "refused, and why: " + why);
        Expect(Throws(() => new AegisMonitor(bad).Dispose()) == Status.Config, "a bad config refused at new");

        using var m = new AegisMonitor(cfg);
        // A bool crosses as one byte: true 6 times out of 20, counted 6.
        for (uint t = 1; t <= 20; t++) m.Accepted(t, 4, t % 3 == 0);
        Expect(m.TryStats(4, false, out var s) && s.Accepted == 20 && s.Anomalies == 6, $"anomalies {s.Anomalies}");
        for (uint t = 21; t <= 140; t++) m.Accepted(t, 4, false);
        Expect(m.TryStats(4, true, out var w) && w.Anomalies == 0 && w.Accepted == 100, "window is the last 100");
        Expect(!m.TryStats(9, false, out _), "no session");
        m.Left(141, 4);
        Expect(!m.TryStats(4, false, out _), "gone after Left");
        Expect(AegisMonitor.Label(Reason.Foresight) == "foresight", AegisMonitor.Label(Reason.Foresight));
        Expect(AegisMonitor.Label((Reason)200) == "unknown", "unknown reason");
        while (m.Poll(out _)) { }
        Expect(!m.Poll(out _), "drained");
    }

    // ---- an engine's own world, as examples/engine.c ----

    static void Engine()
    {
        const float radius = 30f;
        Sees fog = (fx, fy, tx, ty) => MathF.Sqrt((tx - fx) * (tx - fx) + (ty - fy) * (ty - fy)) < radius;
        using var ev = new AegisEvidence();
        using var m = new AegisMonitor();
        var world = new[]
        {
            new Player(1, 0, 0), new Player(2, 0, 200), new Player(3, 50, 0), new Player(4, 50, 200),
        };
        int timed = 0;
        void Shoot(uint tick, Player me, Player target, float off)
        {
            float a = MathF.Atan2(target.Y - me.Y, target.X - me.X) + off;
            var s = ev.Shot(tick, tick, me.Id, me.Alive, MathF.Cos(a), MathF.Sin(a));
            if (s.HasGlimpse) m.Glimpse(tick, me.Id, s);
            if (s.HasAim) m.Shot(tick, me.Id, off < 0.05f, s);
        }
        for (uint tick = 1; tick <= 900; tick++)
        {
            float x = tick % 30 >= 10 ? 20f : 50f;
            world[2].X = x;
            world[3].X = x;
            ev.BeginTick(tick, world, world.Length, fog);
            if (tick % 30 == 10)
            {
                Shoot(tick, world[1], world[3], 0f);
                timed++;
            }
            if (tick % 30 == 25) Shoot(tick, world[0], world[2], 0.1f);
        }
        int flagged1 = 0, flagged2 = 0;
        while (m.Poll(out var a))
        {
            Console.WriteLine($"alert tick={a.Tick} player={a.Player} {AegisMonitor.Label(a.Reason)} value={a.Value:F3} over {a.Samples}");
            if (a.Player == 1) flagged1++;
            if (a.Player == 2 && a.Reason == Reason.Reaction) flagged2++;
        }
        Expect(m.TryStats(2, false, out var st), "player 2 has a session");
        Console.WriteLine($"player 2: {st.Shots} shots, {st.Timed} timed, {st.Fast} fast");
        Expect(st.Timed == timed && st.Fast == timed, "every instant shot timed and fast");
        Expect(flagged1 == 0, "the honest player is not flagged");
        Expect(flagged2 > 0, "the instant player is flagged by reaction");
    }

    // ---- client, without a socket ----

    static void Client()
    {
        Expect(Throws(() => new AegisClient(new string('n', AegisClient.NameMax + 1)).Dispose()) == Status.Arg,
            "a name too long");
        using var c = new AegisClient("riw");
        var buf = new byte[AegisClient.SendMax];
        int n = c.Join(buf);
        Expect(n > 0, "a join");
        Expect(Throws(() => c.Join(new byte[n - 1])) == Status.Buffer, "a short buffer");
        Expect(Throws(() => c.Input(AegisClient.TickNewest, 1, 0, 1, 0, true, buf)) == Status.NotJoined, "not joined");
        Expect(!c.TryPlayerId(out _), "no id yet");
        Expect(c.PlayerCount == 0, "no snapshot yet");
        Expect(c.Receive(new byte[] { 9, 9, 9 }, 3, out _) == Status.Malformed, "garbage is malformed");
        var sealedClient = new AegisClient("riw", new byte[AegisClient.KeysLen]);
        Expect(sealedClient.Receive(new byte[64], 64, out _) == Status.BadSeal, "garbage does not open");
        sealedClient.Dispose();
    }

    // ---- a callback calling back in; a callback that throws ----

    static void Reentry()
    {
        using var ev = new AegisEvidence();
        var world = new[] { new Player(1, 0, 0), new Player(2, 9, 0) };
        var inner = Status.Ok;
        ev.BeginTick(1, world, 2, (fx, fy, tx, ty) =>
        {
            inner = Throws(() => ev.BeginTick(1, world, 0, (a, b, c, d) => true));
            return true;
        });
        Expect(inner == Status.Busy, $"re-entry refused, got {inner}");
        bool threw = false;
        try
        {
            ev.BeginTick(2, world, 2, (fx, fy, tx, ty) => throw new InvalidOperationException("from sees"));
        }
        catch (InvalidOperationException e)
        {
            threw = e.Message == "from sees";
        }
        Expect(threw, "the callback's exception comes back out");
        ev.BeginTick(3, world, 2, (fx, fy, tx, ty) => true);
        var s = ev.Shot(3, 3, 1, true, 1, 0);
        Expect(s.Live && s.HasAim && s.Enemy == 2, "the handle survives both");
        // Disposed from inside sees: freed once the call returns, not during it.
        var gone = new AegisEvidence();
        gone.BeginTick(1, world, 2, (fx, fy, tx, ty) =>
        {
            gone.Dispose();
            return true;
        });
        bool disposed = false;
        try
        {
            gone.Shot(1, 1, 1, true, 1, 0);
        }
        catch (ObjectDisposedException)
        {
            disposed = true;
        }
        Expect(disposed, "disposed after the call");
    }

    // ---- noise, without a relay ----

    static void Noise()
    {
        var pub = Enumerable.Range(9, NoiseHandshake.PublicLen).Select(i => (byte)i).ToArray();
        using (var h = new NoiseHandshake(pub))
        {
            Expect(h.Hello.Length == NoiseHandshake.HelloLen, "a hello");
            Expect(h.Answer(new byte[] { 1, 2, 3 }, 3) == NoiseAnswer.Malformed, "a stray");
            var challenge = new byte[24];
            Array.Copy(h.Hello, challenge, 16);
            BitConverter.GetBytes(0x0123456789abcdefUL).CopyTo(challenge, 16);
            Expect(h.Answer(challenge, challenge.Length) == NoiseAnswer.Challenge && h.Cookie == 0x0123456789abcdefUL,
                "a challenge");
        }
        using (var h = new NoiseHandshake(pub, 0x0123456789abcdefUL))
            Expect(BitConverter.ToUInt64(h.Hello, 16) == 0x0123456789abcdefUL, "the cookie went out");
    }
}
