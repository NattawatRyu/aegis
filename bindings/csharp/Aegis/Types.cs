using System;
using System.Runtime.InteropServices;

namespace Aegis
{
    /// <summary>AEGIS_OK and the AEGIS_ERR_* statuses. Never renumbered.</summary>
    public enum Status
    {
        Ok = 0,
        /// <summary>A required pointer was null (a bug in this binding if seen).</summary>
        Null = -1,
        /// <summary>Config refused; Config.Validate says why.</summary>
        Config = -2,
        /// <summary>A panic was caught; that handle refuses every later call.</summary>
        Panic = -3,
        /// <summary>No live session for that player.</summary>
        NoSession = -4,
        /// <summary>An argument out of range.</summary>
        Arg = -5,
        /// <summary>The buffer was too small; nothing written.</summary>
        Buffer = -6,
        /// <summary>An input before the client was joined.</summary>
        NotJoined = -7,
        /// <summary>That tick is not among the last 16 snapshots.</summary>
        UnknownTick = -8,
        /// <summary>An older tick than an earlier input's.</summary>
        TickRegressed = -9,
        /// <summary>Did not open under this client's keys.</summary>
        BadSeal = -10,
        /// <summary>Not a server message.</summary>
        Malformed = -11,
        /// <summary>Called from inside a sees callback on the same handle.</summary>
        Busy = -12,
    }

    /// <summary>Why a player was flagged. Never renumbered.</summary>
    public enum Reason : byte
    {
        Accuracy = 0,
        AimExact = 1,
        AnomalyRate = 2,
        Reaction = 3,
        Foresight = 4,
    }

    public sealed class AegisException : Exception
    {
        public Status Status { get; }

        public AegisException(Status status, string message = null)
            : base(message ?? $"aegis: {status} ({(int)status})")
        {
            Status = status;
        }

        /// <summary>A count (>= 0) through; a negative status thrown.</summary>
        internal static int Check(int status)
        {
            if (status < 0) throw new AegisException((Status)status);
            return status;
        }
    }

    /// <summary>
    /// Every line the detectors draw. Angles in radians, ReactionFastTicks in
    /// the game's ticks, thresholds are shares in [0, 1). Start from
    /// Config.AtTickRate and change what your own honest telemetry contradicts.
    /// </summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct Config
    {
        public uint AccuracyMinShots;
        public float AccuracyThreshold;
        public float AimExactRad;
        public uint AimExactMinShots;
        public float AimExactThreshold;
        public uint AnomalyMinInputs;
        public float AnomalyThreshold;
        public uint ReactionFastTicks;
        public uint ReactionMinTimed;
        public float ReactionThreshold;
        public float ForesightFitRad;
        public float ForesightClearRad;
        public uint ForesightMinForeseen;
        public float ForesightThreshold;

        public static Config Default()
        {
            AegisException.Check(Native.aegis_config_default(out var c));
            return c;
        }

        /// <summary>The defaults with every tick count scaled to hz.</summary>
        public static Config AtTickRate(uint hz)
        {
            AegisException.Check(Native.aegis_config_at_tick_rate(hz, out var c));
            return c;
        }

        /// <summary>null if accepted, else "field: problem".</summary>
        public string Validate()
        {
            var msg = new byte[256];
            int s = Native.aegis_config_validate(ref this, msg, (UIntPtr)msg.Length);
            if (s == (int)Status.Ok) return null;
            if (s != (int)Status.Config) AegisException.Check(s);
            int n = Array.IndexOf(msg, (byte)0);
            return System.Text.Encoding.UTF8.GetString(msg, 0, n < 0 ? msg.Length : n);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Alert
    {
        /// <summary>Of the record that raised it.</summary>
        public uint Tick;
        public byte Player;
        public Reason Reason;
        /// <summary>What was measured.</summary>
        public float Value;
        /// <summary>The line it crossed.</summary>
        public float Threshold;
        /// <summary>How many samples it was measured over.</summary>
        public uint Samples;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Stats
    {
        public uint Accepted;
        public uint Anomalies;
        public uint Shots;
        public uint Hits;
        public uint Exact;
        public uint Timed;
        public uint Fast;
        public uint Glimpsed;
        public uint Foreseen;
    }

    /// <summary>What a datagram was; fields are read by Kind (see Rx, Event).</summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct Received
    {
        public uint Tick;
        public byte Kind;
        public byte Event;
        /// <summary>Rx.Joined: this client's id.</summary>
        public byte Player;
        public byte Other;
        public byte Damage;
    }

    public static class Rx
    {
        public const byte Joined = 1;
        public const byte Challenge = 2;
        public const byte Snapshot = 3;
        public const byte Event = 4;
    }

    public static class Event
    {
        /// <summary>Player hit Other for Damage.</summary>
        public const byte Hit = 1;
        /// <summary>Player killed by Other.</summary>
        public const byte Death = 2;
        public const byte Join = 3;
        public const byte Leave = 4;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Player
    {
        public float X;
        public float Y;
        public byte Id;
        public byte Health;
        /// <summary>Nonzero: alive. A byte, not a bool, so it crosses as one byte.</summary>
        public byte AliveByte;

        public bool Alive
        {
            get => AliveByte != 0;
            set => AliveByte = value ? (byte)1 : (byte)0;
        }

        public Player(byte id, float x, float y, bool alive = true, byte health = 100)
        {
            X = x;
            Y = y;
            Id = id;
            Health = health;
            AliveByte = alive ? (byte)1 : (byte)0;
        }
    }

    /// <summary>
    /// What one shot says. Live false: no evidence at all. HasAim: feed
    /// AegisMonitor.Shot with AimErr and React once the hit is known.
    /// HasGlimpse: feed AegisMonitor.Glimpse with HasClaimed, Claimed, Ahead
    /// first.
    /// </summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct ShotEvidence
    {
        // C bools: one byte each.
        public byte LiveByte;
        public byte HasAimByte;
        public byte HasGlimpseByte;
        public byte HasClaimedByte;
        /// <summary>The nearest enemy in the shooter's picture.</summary>
        public byte Enemy;
        public float AimErr;
        /// <summary>Negative: not timed.</summary>
        public int React;
        public float Claimed;
        public float Ahead;

        public bool Live => LiveByte != 0;
        public bool HasAim => HasAimByte != 0;
        public bool HasGlimpse => HasGlimpseByte != 0;
        public bool HasClaimed => HasClaimedByte != 0;
    }
}
