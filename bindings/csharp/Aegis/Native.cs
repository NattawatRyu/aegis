// The raw C ABI (crates/ffi/include/aegis.h), one declaration per function.
// Use the wrappers (AegisMonitor, AegisClient, AegisEvidence, NoiseHandshake)
// instead: they own the handles and turn statuses into exceptions.
//
// Two rules every declaration here keeps:
//  - a C `bool` parameter is [MarshalAs(UnmanagedType.U1)]: the default
//    marshals a C# bool as a 4-byte Win32 BOOL;
//  - a struct holds only blittable fields (no C# bool), so it crosses as
//    raw memory, laid out as in C. The crate's test compares every size and
//    offset with Rust's.

using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Aegis
{
    internal static class Native
    {
        // aegis_ffi.dll, libaegis_ffi.so, libaegis_ffi.dylib.
        internal const string Lib = "aegis_ffi";

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        internal delegate int SeesFn(IntPtr ctx, float fx, float fy, float tx, float ty);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern uint aegis_abi_version();

        // ---- Monitor ----

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_config_default(out Config cfg);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_config_at_tick_rate(uint hz, out Config cfg);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_config_validate(ref Config cfg, byte[] msg, UIntPtr len);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_monitor_new(ref Config cfg, out MonitorHandle m);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void aegis_monitor_free(IntPtr m);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_monitor_accepted(MonitorHandle m, uint tick, byte player,
            [MarshalAs(UnmanagedType.U1)] bool anomaly);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_monitor_rejected(MonitorHandle m, uint tick, byte player);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_monitor_shot(MonitorHandle m, uint tick, byte player,
            [MarshalAs(UnmanagedType.U1)] bool hit, float aimErr, int react);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_monitor_glimpse(MonitorHandle m, uint tick, byte player,
            [MarshalAs(UnmanagedType.U1)] bool hasClaimed, float claimed, float ahead);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_monitor_left(MonitorHandle m, uint tick, byte player);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_monitor_poll(MonitorHandle m, out Alert alert);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_monitor_stats(MonitorHandle m, byte player,
            [MarshalAs(UnmanagedType.U1)] bool window, out Stats stats);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern IntPtr aegis_reason_label(byte reason);

        // ---- Client ----

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_client_new(byte[] name, byte[] keys, out ClientHandle c);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void aegis_client_free(IntPtr c);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_client_join(ClientHandle c, byte[] buf, UIntPtr cap);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_client_input(ClientHandle c, uint tick, float moveX, float moveY,
            float aimX, float aimY, [MarshalAs(UnmanagedType.U1)] bool shoot, byte[] buf, UIntPtr cap);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_client_receive(ClientHandle c, byte[] data, UIntPtr len, out Received rx);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_client_player_id(ClientHandle c);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_client_player_count(ClientHandle c);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_client_player(ClientHandle c, uint i, out Player p);

        // ---- Evidence ----

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_evidence_new(float pointBlank, out EvidenceHandle e);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void aegis_evidence_free(IntPtr e);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_evidence_begin_tick(EvidenceHandle e, uint tick, Player[] players,
            UIntPtr n, SeesFn sees, IntPtr ctx);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_evidence_joined(EvidenceHandle e, uint tick, Player[] players,
            UIntPtr n, SeesFn sees, IntPtr ctx, byte id);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_evidence_shot(EvidenceHandle e, uint tick, uint seen, byte shooter,
            [MarshalAs(UnmanagedType.U1)] bool alive, float aimX, float aimY, out ShotEvidence s);

        // ---- Noise (library built with feature `noise` only) ----

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_noise_hello(byte[] relayPublic, ulong cookie, out NoiseHandle n,
            byte[] buf, UIntPtr cap);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int aegis_noise_answer(NoiseHandle n, byte[] data, UIntPtr len, out ulong cookie,
            byte[] keys);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void aegis_noise_free(IntPtr n);
    }

    // A SafeHandle is held for the length of every call that takes it, so a
    // Dispose racing a call (or made from inside a `sees` callback) frees
    // the native handle only once that call has returned.

    internal sealed class MonitorHandle : SafeHandleZeroOrMinusOneIsInvalid
    {
        public MonitorHandle() : base(true) { }

        protected override bool ReleaseHandle()
        {
            Native.aegis_monitor_free(handle);
            return true;
        }
    }

    internal sealed class ClientHandle : SafeHandleZeroOrMinusOneIsInvalid
    {
        public ClientHandle() : base(true) { }

        protected override bool ReleaseHandle()
        {
            Native.aegis_client_free(handle);
            return true;
        }
    }

    internal sealed class EvidenceHandle : SafeHandleZeroOrMinusOneIsInvalid
    {
        public EvidenceHandle() : base(true) { }

        protected override bool ReleaseHandle()
        {
            Native.aegis_evidence_free(handle);
            return true;
        }
    }

    internal sealed class NoiseHandle : SafeHandleZeroOrMinusOneIsInvalid
    {
        public NoiseHandle() : base(true) { }

        protected override bool ReleaseHandle()
        {
            Native.aegis_noise_free(handle);
            return true;
        }
    }
}

namespace AOT
{
    // IL2CPP (Unity) needs a native callback to be a static method marked
    // with an attribute of this name; it matches by name, so an internal
    // copy works without referencing UnityEngine.
    [AttributeUsage(AttributeTargets.Method)]
    internal sealed class MonoPInvokeCallbackAttribute : Attribute
    {
        public MonoPInvokeCallbackAttribute(Type type) { _ = type; }
    }
}
