using System;
using System.Text;

namespace Aegis
{
    /// <summary>
    /// A game client speaking to an Aegis relay or origin. The game owns the
    /// socket: send Join; on Rx.Challenge send Join again (it now carries the
    /// cookie); on Rx.Joined inputs may go. Each input names the tick of the
    /// snapshot it was chosen on (the one displayed); the last 16 are kept.
    /// One thread at a time.
    /// </summary>
    public sealed class AegisClient : IDisposable
    {
        public const int KeysLen = 80;
        public const int NameMax = 32;
        /// <summary>Holds any datagram a client sends.</summary>
        public const int SendMax = 160;
        public const uint TickNewest = uint.MaxValue;

        private readonly ClientHandle _h;

        /// <summary>
        /// name: at most NameMax UTF-8 bytes. keys: KeysLen bytes from the
        /// game's backend (or NoiseHandshake), or null (straight to an origin,
        /// unsealed: lab only).
        /// </summary>
        public AegisClient(string name, byte[] keys = null)
        {
            if (name == null) throw new ArgumentNullException(nameof(name));
            if (keys != null && keys.Length != KeysLen)
                throw new ArgumentException($"keys must be {KeysLen} bytes", nameof(keys));
            var utf8 = Encoding.UTF8.GetBytes(name);
            var z = new byte[utf8.Length + 1];
            Buffer.BlockCopy(utf8, 0, z, 0, utf8.Length);
            AegisException.Check(Native.aegis_client_new(z, keys, out _h));
        }

        /// <summary>Writes the Join datagram into buf; returns its length.</summary>
        public int Join(byte[] buf) => AegisException.Check(Native.aegis_client_join(_h, buf, Cap(buf)));

        /// <summary>
        /// Writes one input into buf; returns its length. tick: the snapshot it
        /// was chosen on, or TickNewest.
        /// </summary>
        public int Input(uint tick, float moveX, float moveY, float aimX, float aimY, bool shoot, byte[] buf) =>
            AegisException.Check(Native.aegis_client_input(_h, tick, moveX, moveY, aimX, aimY, shoot, buf, Cap(buf)));

        /// <summary>
        /// Reads the first len bytes of data. Status.Ok with rx written;
        /// Status.BadSeal or Status.Malformed for a datagram that is not this
        /// client's (drop it). Other failures throw.
        /// </summary>
        public Status Receive(byte[] data, int len, out Received rx)
        {
            if (data == null) throw new ArgumentNullException(nameof(data));
            if (len < 0 || len > data.Length) throw new ArgumentOutOfRangeException(nameof(len));
            int s = Native.aegis_client_receive(_h, data, (UIntPtr)len, out rx);
            if (s == (int)Status.BadSeal || s == (int)Status.Malformed) return (Status)s;
            AegisException.Check(s);
            return Status.Ok;
        }

        /// <summary>false until joined.</summary>
        public bool TryPlayerId(out byte id)
        {
            int s = Native.aegis_client_player_id(_h);
            id = s >= 0 ? (byte)s : (byte)0;
            if (s == (int)Status.NotJoined) return false;
            AegisException.Check(s);
            return true;
        }

        /// <summary>The newest snapshot's players.</summary>
        public int PlayerCount => AegisException.Check(Native.aegis_client_player_count(_h));

        public Player GetPlayer(int i)
        {
            if (i < 0) throw new ArgumentOutOfRangeException(nameof(i));
            AegisException.Check(Native.aegis_client_player(_h, (uint)i, out var p));
            return p;
        }

        /// <summary>
        /// Draw delay ticks behind the newest snapshot from now on (clamped to
        /// 0..6). Each frame: TryView, draw ViewPlayer(0..Players), send what
        /// was chosen on it with InputDrawn. Called again, starts over.
        /// </summary>
        public void Interpolate(double delay) => AegisException.Check(Native.aegis_client_interpolate(_h, delay));

        /// <summary>
        /// dt ticks have passed since the last frame (>= 0): the frame to draw.
        /// false until interpolating and a snapshot has landed.
        /// </summary>
        public bool TryView(double dt, out View view)
        {
            int s = Native.aegis_client_view(_h, dt, out view);
            if (s == (int)Status.NotReady) return false;
            AegisException.Check(s);
            return true;
        }

        /// <summary>Player i of the last frame, placed at its moment.</summary>
        public Player ViewPlayer(int i)
        {
            if (i < 0) throw new ArgumentOutOfRangeException(nameof(i));
            AegisException.Check(Native.aegis_client_view_player(_h, (uint)i, out var p));
            return p;
        }

        /// <summary>
        /// Writes an input chosen on the last frame into buf; returns its
        /// length. It claims the frame's tick and its offset, so the server
        /// judges the shot in the picture on screen.
        /// </summary>
        public int InputDrawn(float moveX, float moveY, float aimX, float aimY, bool shoot, byte[] buf) =>
            AegisException.Check(Native.aegis_client_input_drawn(_h, moveX, moveY, aimX, aimY, shoot, buf, Cap(buf)));

        /// <summary>How steadily the frames drawn so far advanced; throws unless interpolating.</summary>
        public Pace Pace
        {
            get
            {
                AegisException.Check(Native.aegis_client_pace(_h, out var p));
                return p;
            }
        }

        /// <summary>
        /// Predict this player's position from now on, in an arena with walls
        /// (null: none) — the server's own. Exact against aegis-server's
        /// movement; an engine with its own physics predicts with that.
        /// </summary>
        public void Predict(Wall[] walls) =>
            AegisException.Check(Native.aegis_client_predict(_h, walls, (UIntPtr)(walls?.Length ?? 0)));

        /// <summary>false until predicting and a snapshot has shown this player.</summary>
        public bool TryPredicted(out float x, out float y)
        {
            int s = Native.aegis_client_predicted(_h, out x, out y);
            if (s == (int)Status.NotReady) return false;
            AegisException.Check(s);
            return true;
        }

        /// <summary>How the predictions compared with the server; throws unless predicting.</summary>
        public PredictStats PredictionStats
        {
            get
            {
                AegisException.Check(Native.aegis_client_prediction_stats(_h, out var s));
                return s;
            }
        }

        public void Dispose() => _h.Dispose();

        private static UIntPtr Cap(byte[] buf) =>
            (UIntPtr)(buf ?? throw new ArgumentNullException(nameof(buf))).Length;
    }
}
