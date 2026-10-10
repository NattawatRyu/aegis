using System;

namespace Aegis
{
    public enum NoiseAnswer
    {
        /// <summary>Cookie is set: dispose, start a new NoiseHandshake with it.</summary>
        Challenge = 1,
        /// <summary>Keys is set: hand them to AegisClient.</summary>
        Welcome = 2,
        /// <summary>Not an answer to a hello; nothing changed. Read the next.</summary>
        Malformed = -11,
        /// <summary>A welcome that did not open; this handshake is spent. Start again.</summary>
        BadSeal = -10,
    }

    /// <summary>
    /// Keys from a relay that is its own backend (Noise NK), knowing only its
    /// static public key. Needs aegis_ffi built with feature `noise`; without
    /// it the first call throws EntryPointNotFoundException.
    ///
    ///   using (var h = new NoiseHandshake(pub)) send(h.Hello); h.Answer(...) == Challenge
    ///   using (var h = new NoiseHandshake(pub, cookie)) send(h.Hello); h.Answer(...) == Welcome
    ///   new AegisClient(name, h.Keys)
    /// </summary>
    public sealed class NoiseHandshake : IDisposable
    {
        public const int PublicLen = 32;
        public const int HelloLen = 72;

        private readonly NoiseHandle _h;

        /// <summary>The datagram to send to the relay.</summary>
        public byte[] Hello { get; }

        /// <summary>Set by a Challenge answer.</summary>
        public ulong Cookie { get; private set; }

        /// <summary>Set by a Welcome answer: AegisClient.KeysLen bytes.</summary>
        public byte[] Keys { get; private set; }

        /// <summary>cookie 0: none yet.</summary>
        public NoiseHandshake(byte[] relayPublic, ulong cookie = 0)
        {
            if (relayPublic == null || relayPublic.Length != PublicLen)
                throw new ArgumentException($"the relay's public key is {PublicLen} bytes", nameof(relayPublic));
            var buf = new byte[HelloLen];
            int n = AegisException.Check(Native.aegis_noise_hello(relayPublic, cookie, out _h, buf, (UIntPtr)buf.Length));
            Hello = n == buf.Length ? buf : buf.AsSpan(0, n).ToArray();
        }

        /// <summary>Read the first len bytes of a datagram from the relay.</summary>
        public NoiseAnswer Answer(byte[] data, int len)
        {
            if (data == null) throw new ArgumentNullException(nameof(data));
            if (len < 0 || len > data.Length) throw new ArgumentOutOfRangeException(nameof(len));
            var keys = new byte[AegisClient.KeysLen];
            int s = Native.aegis_noise_answer(_h, data, (UIntPtr)len, out ulong cookie, keys);
            switch (s)
            {
                case (int)NoiseAnswer.Challenge:
                    Cookie = cookie;
                    return NoiseAnswer.Challenge;
                case (int)NoiseAnswer.Welcome:
                    Keys = keys;
                    return NoiseAnswer.Welcome;
                case (int)NoiseAnswer.Malformed:
                case (int)NoiseAnswer.BadSeal:
                    return (NoiseAnswer)s;
                default:
                    AegisException.Check(s);
                    throw new AegisException((Status)s);
            }
        }

        public void Dispose() => _h.Dispose();
    }
}
