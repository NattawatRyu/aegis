// Aegis inside a Godot game: the game's own world, its own physics, its own
// hit resolution; Aegis only measures. Each physics tick the game hands
// AegisEvidence its players and a line-of-sight test (a Godot raycast
// against the walls), asks it for each shot's evidence, and feeds that to
// an AegisMonitor, whose alerts are shown on screen.
//
// Free-for-all in a walled arena. Player 1 is you (WASD, mouse, left
// click). Bots, honest:
//   honest     reacts like a person: 270-530 ms, 0.05 rad off;
//   pro        the hardest honest player to tell from a cheat: 130-270 ms,
//              67-133 ms when it heard the enemy coming (within 200 px as
//              it appears), 0.03 rad off.
// Bots, cheating:
//   aimbot     fires the tick an enemy comes into sight, dead on;
//   humanized  an aimbot that waits like a person (200-400 ms) and adds
//              0.012 rad of noise: never "exact", never "fast";
//   trigger    aims like a person (turns toward the target, jittery), but
//              fires the instant its aim crosses an enemy.
//
// Headless, as a test:
//   godot --headless --fixed-fps 30 --path demos/godot -- --ticks 9000 --seed 7 [--roster honest,pro,aimbot]
// prints every alert, each player's stats and, per cheat, CAUGHT or MISSED;
// exits 0 if every plain aimbot was flagged and no honest or pro bot was,
// else 1. A missed humanized or trigger bot is reported, not failed: what
// Aegis does not catch yet is a finding, not a broken build.

using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Runtime.InteropServices;
using Aegis;
using Godot;

public partial class Main : Node2D
{
    const int TickRate = 30;
    const float HitRadius = 12f;
    const float Speed = 90f; // px per second
    const uint WallMask = 1;
    static readonly Vector2 Arena = new Vector2(960, 640);

    enum Kind { Human, Honest, Pro, Aimbot, Humanized, Trigger }

    static bool Cheats(Kind k) => k is Kind.Aimbot or Kind.Humanized or Kind.Trigger;

    sealed class Actor
    {
        public byte Id;
        public Kind Kind;
        public Vector2 Pos;
        public Vector2 Goal;
        public float Aim;
        // What Aegis measured on its shots, for the report.
        public readonly List<float> AimErrs = new();
        public readonly List<int> Reacts = new();
        public int Health = 100;
        public uint DeadUntil;
        public uint ReadyAt;
        // Per enemy: the tick it came into sight, and the reaction delay
        // drawn for that engagement.
        public readonly Dictionary<byte, (uint since, uint delay)> Seen = new();
        public bool Alive => Health > 0;
    }

    readonly List<Actor> _actors = new();
    readonly List<(Vector2 from, Vector2 to, float life)> _tracers = new();
    readonly List<string> _log = new();
    readonly Rect2[] _walls =
    {
        new Rect2(200, 120, 40, 260), new Rect2(720, 260, 40, 260), new Rect2(380, 300, 200, 40),
        new Rect2(440, 80, 80, 100), new Rect2(440, 460, 80, 100), new Rect2(80, 470, 160, 30),
        new Rect2(720, 120, 160, 30),
    };

    AegisEvidence _evidence;
    AegisMonitor _monitor;
    Player[] _players = Array.Empty<Player>();
    PhysicsDirectSpaceState2D _space;
    Sees _sees;
    Label _hud;
    Random _rng;
    uint _tick;
    uint _stopAt;
    readonly HashSet<byte> _flagged = new();

    static Main()
    {
        // The native library: from AEGIS_FFI_DIR (the crate's test points it
        // at the build it is testing), res://native/ in the project, or next
        // to the executable in an export.
        NativeLibrary.SetDllImportResolver(typeof(AegisMonitor).Assembly, (name, _, _) =>
        {
            if (name != "aegis_ffi") return IntPtr.Zero;
            string file = OperatingSystem.IsWindows() ? "aegis_ffi.dll"
                : OperatingSystem.IsMacOS() ? "libaegis_ffi.dylib" : "libaegis_ffi.so";
            var dirs = new[]
            {
                OS.GetEnvironment("AEGIS_FFI_DIR"), ProjectSettings.GlobalizePath("res://native"),
                OS.GetExecutablePath().GetBaseDir(),
            };
            foreach (var dir in dirs.Where(d => !string.IsNullOrEmpty(d)))
            {
                var path = System.IO.Path.Combine(dir, file);
                if (System.IO.File.Exists(path)) return NativeLibrary.Load(path);
            }
            return IntPtr.Zero;
        });
    }

    public override void _Ready()
    {
        Engine.PhysicsTicksPerSecond = TickRate;
        var args = ParseArgs(OS.GetCmdlineUserArgs());
        _rng = new Random(args.TryGetValue("seed", out var s) ? int.Parse(s) : System.Environment.TickCount);
        _stopAt = args.TryGetValue("ticks", out var t) ? uint.Parse(t) : 0;

        foreach (var r in _walls)
        {
            var body = new StaticBody2D { CollisionLayer = WallMask, Position = r.GetCenter() };
            body.AddChild(new CollisionShape2D { Shape = new RectangleShape2D { Size = r.Size } });
            AddChild(body);
        }
        _hud = new Label { Position = new Vector2(8, 8) };
        _hud.AddThemeFontSizeOverride("font_size", 13);
        AddChild(_hud);

        try
        {
            _evidence = new AegisEvidence(HitRadius);
            var cfg = Aegis.Config.AtTickRate(TickRate);
            // A game tunes its own lines; these let a sweep try others.
            if (args.TryGetValue("far-rad", out var fr)) cfg.FarAimRad = float.Parse(fr, CultureInfo.InvariantCulture);
            if (args.TryGetValue("far-line", out var fl)) cfg.FarAimThreshold = float.Parse(fl, CultureInfo.InvariantCulture);
            _monitor = new AegisMonitor(cfg);
        }
        catch (DllNotFoundException)
        {
            _hud.Text = "aegis_ffi not found: cargo build -p aegis-ffi --release, copy it to demos/godot/native/";
            GD.PushError(_hud.Text);
            SetPhysicsProcess(false);
            if (_stopAt > 0) GetTree().Quit(2);
            return;
        }
        _sees = Sees;

        // A human only when someone is there to play.
        bool headless = DisplayServer.GetName() == "headless";
        byte id = 1;
        if (!headless) _actors.Add(Spawn(id++, Kind.Human));
        var roster = args.TryGetValue("roster", out var names)
            ? names.Split(',').Select(k => Enum.Parse<Kind>(k, ignoreCase: true)).ToArray()
            : new[] { Kind.Honest, Kind.Honest, Kind.Honest, Kind.Honest, Kind.Aimbot, Kind.Honest };
        foreach (var k in roster) _actors.Add(Spawn(id++, k));
    }

    Actor Spawn(byte id, Kind kind) => new Actor { Id = id, Kind = kind, Pos = FreeSpot(), Goal = FreeSpot() };

    Vector2 FreeSpot()
    {
        while (true)
        {
            var p = new Vector2(30 + (float)_rng.NextDouble() * (Arena.X - 60), 30 + (float)_rng.NextDouble() * (Arena.Y - 60));
            if (!_walls.Any(w => w.Grow(HitRadius + 4).HasPoint(p))) return p;
        }
    }

    // One query, reused: a new PhysicsRayQueryParameters2D per ray is a
    // RefCounted object per ray (tens of thousands a minute here) left to
    // the finalizer, and finalizing them at shutdown crashed Godot 4.7.2
    // (FATAL in csharp_script.cpp, _instance_binding_free_callback) after
    // the run had finished: a nonzero exit on a passing run.
    readonly PhysicsRayQueryParameters2D _ray = new() { CollisionMask = WallMask };

    bool Sees(float fx, float fy, float tx, float ty)
    {
        _ray.From = new Vector2(fx, fy);
        _ray.To = new Vector2(tx, ty);
        using var hit = _space.IntersectRay(_ray);
        return hit.Count == 0;
    }

    bool Sees(Actor a, Actor b) => Sees(a.Pos.X, a.Pos.Y, b.Pos.X, b.Pos.Y);

    public override void _PhysicsProcess(double delta)
    {
        _tick++;
        _space = GetWorld2D().DirectSpaceState;
        float dt = 1f / TickRate;

        foreach (var a in _actors)
        {
            if (!a.Alive && _tick >= a.DeadUntil)
            {
                a.Health = 100;
                a.Pos = FreeSpot();
                a.Seen.Clear();
            }
            if (a.Alive) Move(a, dt);
        }

        // The picture this tick's shots are judged in: where everyone is now.
        if (_players.Length != _actors.Count) _players = new Player[_actors.Count];
        for (int i = 0; i < _actors.Count; i++)
        {
            var a = _actors[i];
            _players[i] = new Player(a.Id, a.Pos.X, a.Pos.Y, a.Alive, (byte)Math.Max(0, a.Health));
        }
        _evidence.BeginTick(_tick, _players, _players.Length, _sees);

        foreach (var a in _actors.Where(a => a.Alive))
        {
            if (a.Kind == Kind.Human)
            {
                a.Aim = (GetGlobalMousePosition() - a.Pos).Angle();
                if (Input.IsMouseButtonPressed(MouseButton.Left) && _tick >= a.ReadyAt) Fire(a, a.Aim);
                continue;
            }
            var target = Choose(a);
            if (a.Kind == Kind.Trigger)
            {
                Trigger(a, target);
                continue;
            }
            if (target == null || _tick < a.ReadyAt) continue;
            float aim = (target.Pos - a.Pos).Angle();
            aim += Gaussian() * a.Kind switch
            {
                Kind.Honest => 0.05f,
                Kind.Pro => 0.03f,
                Kind.Humanized => 0.012f,
                _ => 0f,
            };
            Fire(a, aim);
        }

        while (_monitor.Poll(out var alert))
        {
            var who = _actors.First(x => x.Id == alert.Player);
            _flagged.Add(alert.Player);
            var line = $"tick {alert.Tick}: player {alert.Player} ({who.Kind}) {AegisMonitor.Label(alert.Reason)} {alert.Value:F2} over {alert.Samples}";
            _log.Insert(0, line);
            GD.Print("alert " + line);
        }
        if (_log.Count > 12) _log.RemoveRange(12, _log.Count - 12);
        for (int i = _tracers.Count - 1; i >= 0; i--)
        {
            var tr = _tracers[i];
            if ((tr.life -= dt) <= 0) _tracers.RemoveAt(i);
            else _tracers[i] = tr;
        }
        _hud.Text = $"tick {_tick}   you: WASD + mouse   alerts (evidence for a reviewer, not a ban):\n" + string.Join("\n", _log);
        QueueRedraw();

        if (_stopAt > 0 && _tick >= _stopAt) Finish();
    }

    void Move(Actor a, float dt)
    {
        Vector2 dir;
        if (a.Kind == Kind.Human)
        {
            dir = Input.GetVector("ui_left", "ui_right", "ui_up", "ui_down");
            if (Input.IsKeyPressed(Key.A)) dir.X -= 1;
            if (Input.IsKeyPressed(Key.D)) dir.X += 1;
            if (Input.IsKeyPressed(Key.W)) dir.Y -= 1;
            if (Input.IsKeyPressed(Key.S)) dir.Y += 1;
            dir = dir.LimitLength(1);
        }
        else
        {
            if (a.Pos.DistanceTo(a.Goal) < 8) a.Goal = FreeSpot();
            dir = (a.Goal - a.Pos).Normalized();
        }
        var next = (a.Pos + dir * Speed * dt).Clamp(new Vector2(HitRadius, HitRadius), Arena - new Vector2(HitRadius, HitRadius));
        if (_walls.Any(w => w.Grow(HitRadius).HasPoint(next)))
        {
            if (a.Kind != Kind.Human) a.Goal = FreeSpot();
            return;
        }
        a.Pos = next;
    }

    /// The enemy a bot fires at now, if any: honest bots once their
    /// reaction delay for that engagement has passed, aimbots at once.
    Actor Choose(Actor a)
    {
        Actor best = null;
        foreach (var e in _actors)
        {
            if (e == a) continue;
            bool visible = e.Alive && Sees(a, e);
            if (!visible)
            {
                a.Seen.Remove(e.Id);
                continue;
            }
            if (!a.Seen.TryGetValue(e.Id, out var s))
            {
                a.Seen[e.Id] = s = (_tick, Delay(a, e));
            }
            if (_tick - s.since >= s.delay && (best == null || a.Pos.DistanceTo(e.Pos) < a.Pos.DistanceTo(best.Pos)))
                best = e;
        }
        return best;
    }

    /// Ticks from an enemy coming into sight to the first shot at it.
    uint Delay(Actor a, Actor e) => a.Kind switch
    {
        Kind.Aimbot => 0u,
        // Heard coming: it is close as it appears, and the pro was ready.
        Kind.Pro => a.Pos.DistanceTo(e.Pos) < 200 ? (uint)_rng.Next(2, 5) : (uint)_rng.Next(4, 9),
        Kind.Humanized => (uint)_rng.Next(6, 13),
        // Honest, and the trigger bot's human hand.
        _ => (uint)_rng.Next(8, 17),
    };

    /// A human hand on the mouse — it turns toward the target at most
    /// 0.2 rad a tick, jittery — and a program on the trigger: a shot the
    /// tick the aim crosses an enemy.
    void Trigger(Actor a, Actor target)
    {
        if (target != null)
        {
            float want = (target.Pos - a.Pos).Angle();
            float turn = Mathf.Clamp(Mathf.AngleDifference(a.Aim, want), -0.2f, 0.2f);
            a.Aim = Mathf.Wrap(a.Aim + turn + Gaussian() * 0.04f, -Mathf.Pi, Mathf.Pi);
        }
        if (_tick < a.ReadyAt) return;
        var dir = Vector2.FromAngle(a.Aim);
        foreach (var e in _actors)
        {
            if (e == a || !e.Alive) continue;
            var to = e.Pos - a.Pos;
            if (to.Dot(dir) > 0 && Mathf.Abs(to.Cross(dir)) < HitRadius * 0.8f && Sees(a, e))
            {
                Fire(a, a.Aim);
                return;
            }
        }
    }

    /// The game resolves the shot: the first player along the aim within
    /// HitRadius of it, not behind a wall. Aegis is told what happened.
    void Fire(Actor a, float aim)
    {
        a.ReadyAt = _tick + 10;
        var dir = Vector2.FromAngle(aim);
        var ev = _evidence.Shot(_tick, _tick, a.Id, a.Alive, dir.X, dir.Y);

        Actor hit = null;
        float hitAt = float.MaxValue;
        foreach (var e in _actors)
        {
            if (e == a || !e.Alive) continue;
            var to = e.Pos - a.Pos;
            float along = to.Dot(dir);
            if (along <= 0 || Mathf.Abs(to.Cross(dir)) > HitRadius || along >= hitAt || !Sees(a, e)) continue;
            hit = e;
            hitAt = along;
        }
        if (ev.HasGlimpse) _monitor.Glimpse(_tick, a.Id, ev);
        if (ev.HasAim)
        {
            _monitor.Shot(_tick, a.Id, hit != null, ev);
            a.AimErrs.Add(ev.AimErr);
            if (ev.React >= 0) a.Reacts.Add(ev.React);
        }
        _tracers.Add((a.Pos, a.Pos + dir * (hit != null ? hitAt : 1200), 0.15f));
        if (hit != null && (hit.Health -= 34) <= 0) hit.DeadUntil = _tick + 2 * TickRate;
    }

    float Gaussian()
    {
        double u = 1 - _rng.NextDouble(), v = _rng.NextDouble();
        return (float)(Math.Sqrt(-2 * Math.Log(u)) * Math.Cos(2 * Math.PI * v));
    }

    public override void _Draw()
    {
        DrawRect(new Rect2(Vector2.Zero, Arena), new Color(0.08f, 0.09f, 0.11f));
        foreach (var w in _walls) DrawRect(w, new Color(0.35f, 0.37f, 0.42f));
        foreach (var (from, to, life) in _tracers) DrawLine(from, to, new Color(1, 0.9f, 0.4f, life / 0.15f), 1.5f);
        foreach (var a in _actors)
        {
            var c = a.Kind switch
            {
                Kind.Human => new Color(0.3f, 0.8f, 1f),
                Kind.Aimbot => new Color(1f, 0.35f, 0.3f),
                Kind.Humanized => new Color(1f, 0.6f, 0.2f),
                Kind.Trigger => new Color(0.9f, 0.4f, 0.9f),
                Kind.Pro => new Color(0.95f, 0.95f, 0.5f),
                _ => new Color(0.5f, 0.9f, 0.5f),
            };
            if (!a.Alive)
            {
                DrawArc(a.Pos, HitRadius, 0, Mathf.Tau, 24, c with { A = 0.3f });
                continue;
            }
            DrawCircle(a.Pos, HitRadius, c);
            if (_flagged.Contains(a.Id)) DrawArc(a.Pos, HitRadius + 5, 0, Mathf.Tau, 24, new Color(1, 0.2f, 0.2f), 2);
            DrawString(ThemeDB.FallbackFont, a.Pos + new Vector2(-4, 5), a.Id.ToString(), fontSize: 12, modulate: Colors.Black);
        }
    }

    void Finish()
    {
        SetPhysicsProcess(false);
        bool ok = true;
        foreach (var a in _actors)
        {
            _monitor.TryStats(a.Id, false, out var s);
            bool flagged = _flagged.Contains(a.Id);
            GD.Print($"player {a.Id} {a.Kind}: {s.Shots} shots, {s.Hits} hits, {s.Timed} timed, {s.Fast} fast, " +
                     $"{s.Exact} exact, aim err median {Pct(a.AimErrs, 0.5):F4} p90 {Pct(a.AimErrs, 0.9):F4} rad, " +
                     $"react median {Pct(a.Reacts.Select(x => (float)x).ToList(), 0.5):F1} ticks, " +
                     $"far {s.FarInside}/{s.Far} inside, flagged {flagged}");
            if (a.Kind == Kind.Aimbot && !flagged) ok = false;
            if (a.Kind is Kind.Honest or Kind.Pro && flagged) ok = false;
            if (Cheats(a.Kind)) GD.Print($"cheat {a.Kind} (player {a.Id}): {(flagged ? "CAUGHT" : "MISSED")}");
        }
        GD.Print(ok ? "VERDICT ok" : "VERDICT wrong");
        GetTree().Quit(ok ? 0 : 1);
    }

    public override void _ExitTree()
    {
        _evidence?.Dispose();
        _monitor?.Dispose();
        _ray.Dispose();
    }

    static float Pct(List<float> xs, double p)
    {
        if (xs.Count == 0) return float.NaN;
        var s = xs.OrderBy(x => x).ToList();
        return s[(int)Math.Min(s.Count - 1, Math.Floor(p * s.Count))];
    }

    static Dictionary<string, string> ParseArgs(string[] args)
    {
        var d = new Dictionary<string, string>();
        for (int i = 0; i + 1 < args.Length; i += 2)
            if (args[i].StartsWith("--")) d[args[i].Substring(2)] = args[i + 1];
        return d;
    }
}
