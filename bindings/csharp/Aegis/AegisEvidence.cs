using System;
using System.Runtime.InteropServices;

namespace Aegis
{
    /// <summary>Can a player standing at (fx, fy) see one standing at (tx, ty)?</summary>
    public delegate bool Sees(float fx, float fy, float tx, float ty);

    /// <summary>
    /// What the Aegis server measures about each shot, over the game's own
    /// world. Per tick: BeginTick with every player where this tick's
    /// snapshots show them; Joined for a player admitted mid-tick; Shot per
    /// shot, in resolution order. Feed each shot's evidence to an
    /// AegisMonitor: Glimpse if HasGlimpse, then, once the hit is known,
    /// Shot if HasAim.
    ///
    /// `seen` is the tick of the snapshot the input was chosen on. The game
    /// must prove it (a MAC'd per-snapshot proof the client echoes), keep it
    /// within the last 16 ticks and never let it go back: an unproven `seen`
    /// lets a client pick the picture it is judged in. One thread at a time.
    /// </summary>
    public sealed class AegisEvidence : IDisposable
    {
        private readonly EvidenceHandle _h;

        /// <summary>
        /// pointBlank: the game's hitbox radius in its units (an enemy that
        /// close is no evidence of aim); 0 for the lab's.
        /// </summary>
        public AegisEvidence(float pointBlank = 0f)
        {
            AegisException.Check(Native.aegis_evidence_new(pointBlank, out _h));
        }

        /// <summary>
        /// Start of a tick: the first count of players (all of them, alive or
        /// dead, ids unique), in an order that stays the same from tick to
        /// tick. sees is called during this call only; an exception it throws
        /// is rethrown here once the native call has returned.
        /// </summary>
        public void BeginTick(uint tick, Player[] players, int count, Sees sees) =>
            WithSees(players, count, sees, (n, ctx) =>
                Native.aegis_evidence_begin_tick(_h, tick, players, n, Thunk, ctx));

        /// <summary>Player id was admitted mid-tick: players is the world with it in.</summary>
        public void Joined(uint tick, Player[] players, int count, Sees sees, byte id) =>
            WithSees(players, count, sees, (n, ctx) =>
                Native.aegis_evidence_joined(_h, tick, players, n, Thunk, ctx, id));

        /// <summary>
        /// A shot by shooter at (aimX, aimY), chosen on the snapshot of tick
        /// seen and resolved on tick. alive: the shooter is alive now.
        /// </summary>
        public ShotEvidence Shot(uint tick, uint seen, byte shooter, bool alive, float aimX, float aimY)
        {
            AegisException.Check(Native.aegis_evidence_shot(_h, tick, seen, shooter, alive, aimX, aimY, out var s));
            return s;
        }

        public void Dispose() => _h.Dispose();

        // ---- the callback ----

        private sealed class Call
        {
            public Sees Sees;
            public Exception Error;
        }

        // One static delegate for the life of the process: never collected
        // while native code holds it, and a static method, as IL2CPP needs.
        private static readonly Native.SeesFn Thunk = SeesThunk;

        [AOT.MonoPInvokeCallback(typeof(Native.SeesFn))]
        private static int SeesThunk(IntPtr ctx, float fx, float fy, float tx, float ty)
        {
            var call = (Call)GCHandle.FromIntPtr(ctx).Target;
            if (call.Error != null) return 0;
            try
            {
                return call.Sees(fx, fy, tx, ty) ? 1 : 0;
            }
            catch (Exception e)
            {
                // Never unwind through native frames.
                call.Error = e;
                return 0;
            }
        }

        private static void WithSees(Player[] players, int count, Sees sees, Func<UIntPtr, IntPtr, int> native)
        {
            if (players == null) throw new ArgumentNullException(nameof(players));
            if (sees == null) throw new ArgumentNullException(nameof(sees));
            if (count < 0 || count > players.Length) throw new ArgumentOutOfRangeException(nameof(count));
            var call = new Call { Sees = sees };
            var gc = GCHandle.Alloc(call);
            int s;
            try
            {
                s = native((UIntPtr)count, GCHandle.ToIntPtr(gc));
            }
            finally
            {
                gc.Free();
            }
            // The tick went on with "not seen" for every pair after the throw,
            // so its evidence is not to be trusted: the caller sees the throw.
            if (call.Error != null)
                System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(call.Error).Throw();
            AegisException.Check(s);
        }
    }
}
