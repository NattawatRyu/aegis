using System;
using System.Runtime.InteropServices;

namespace Aegis
{
    /// <summary>
    /// The online detector. Write the records the game's server already knows
    /// (an input accepted or rejected, a shot, a glimpse, a player gone) and
    /// poll alerts. A flag is evidence for a human reviewer, not a ban. One
    /// thread at a time.
    /// </summary>
    public sealed class AegisMonitor : IDisposable
    {
        public const uint AbiVersion = 2;

        private readonly MonitorHandle _h;

        /// <summary>The library's ABI version; must equal AbiVersion.</summary>
        public static uint LibraryAbiVersion => Native.aegis_abi_version();

        public AegisMonitor() : this(Config.Default()) { }

        public AegisMonitor(Config config)
        {
            if (LibraryAbiVersion != AbiVersion)
                throw new InvalidOperationException(
                    $"aegis_ffi ABI {LibraryAbiVersion}, this binding expects {AbiVersion}");
            int s = Native.aegis_monitor_new(ref config, out _h);
            if (s == (int)Status.Config) throw new AegisException(Status.Config, "config refused: " + config.Validate());
            AegisException.Check(s);
        }

        /// <summary>Each record returns the number of alerts it raised; read them with Poll.</summary>
        public int Accepted(uint tick, byte player, bool anomaly) =>
            AegisException.Check(Native.aegis_monitor_accepted(_h, tick, player, anomaly));

        public int Rejected(uint tick, byte player) =>
            AegisException.Check(Native.aegis_monitor_rejected(_h, tick, player));

        /// <summary>
        /// aimErr: radians from the aim to the nearest enemy. react: ticks from
        /// that enemy coming into sight; negative when not timed. size: that
        /// enemy's angular radius in radians, asin(hitbox radius / distance).
        /// </summary>
        public int Shot(uint tick, byte player, bool hit, float aimErr, int react, float size) =>
            AegisException.Check(Native.aegis_monitor_shot(_h, tick, player, hit, aimErr, react, size));

        /// <summary>A shot's evidence as AegisEvidence measured it, once the hit is known.</summary>
        public int Shot(uint tick, byte player, bool hit, in ShotEvidence s) =>
            Shot(tick, player, hit, s.AimErr, s.React, s.Size);

        public int Glimpse(uint tick, byte player, bool hasClaimed, float claimed, float ahead) =>
            AegisException.Check(Native.aegis_monitor_glimpse(_h, tick, player, hasClaimed, claimed, ahead));

        public int Glimpse(uint tick, byte player, in ShotEvidence s) =>
            Glimpse(tick, player, s.HasClaimed, s.Claimed, s.Ahead);

        public int Left(uint tick, byte player) => AegisException.Check(Native.aegis_monitor_left(_h, tick, player));

        /// <summary>true with alert written, false when none is waiting.</summary>
        public bool Poll(out Alert alert) => AegisException.Check(Native.aegis_monitor_poll(_h, out alert)) == 1;

        /// <summary>false when the player has no live session. window: the last 100 samples of each kind.</summary>
        public bool TryStats(byte player, bool window, out Stats stats)
        {
            int s = Native.aegis_monitor_stats(_h, player, window, out stats);
            if (s == (int)Status.NoSession) return false;
            AegisException.Check(s);
            return true;
        }

        /// <summary>"unknown" for a value outside Reason.</summary>
        public static string Label(Reason reason) => Marshal.PtrToStringAnsi(Native.aegis_reason_label((byte)reason));

        public void Dispose() => _h.Dispose();
    }
}
