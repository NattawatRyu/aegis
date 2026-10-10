/*
 * aegis.h — C ABI of the Aegis online detector (crate aegis-ffi).
 *
 * An engine with its own simulation writes the records its server already
 * knows — an input accepted or rejected, a shot, a glimpse, a player gone —
 * and polls alerts. A flag is evidence for a human reviewer, not a ban.
 *
 * Status: AEGIS_OK (0) or a negative AEGIS_ERR_*. Functions that count
 * return the count (>= 0) instead of AEGIS_OK.
 *
 * Threads: a monitor is used by one thread at a time; different monitors
 * are independent. Memory: free a monitor with aegis_monitor_free only.
 *
 * Held to the Rust source by a test that compiles examples/monitor.c with
 * this header and compares every size and offset.
 */
#ifndef AEGIS_H
#define AEGIS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define AEGIS_ABI_VERSION 1u

#define AEGIS_OK 0
#define AEGIS_ERR_NULL (-1)       /* a required pointer was null */
#define AEGIS_ERR_CONFIG (-2)     /* config refused; aegis_config_validate says why */
#define AEGIS_ERR_PANIC (-3)      /* caught a panic; that monitor refuses every later call */
#define AEGIS_ERR_NO_SESSION (-4) /* no live session for that player */
#define AEGIS_ERR_ARG (-5)        /* an argument out of range */

/* Never renumbered. */
#define AEGIS_REASON_ACCURACY 0
#define AEGIS_REASON_AIM_EXACT 1
#define AEGIS_REASON_ANOMALY_RATE 2
#define AEGIS_REASON_REACTION 3
#define AEGIS_REASON_FORESIGHT 4

/* Every line the detectors draw. Angles in radians, fast_ticks in the
 * game's ticks, thresholds are shares in [0, 1). Start from
 * aegis_config_at_tick_rate and change what your own honest telemetry
 * contradicts. */
typedef struct AegisConfig {
    uint32_t accuracy_min_shots;
    float accuracy_threshold;
    float aim_exact_rad;
    uint32_t aim_exact_min_shots;
    float aim_exact_threshold;
    uint32_t anomaly_min_inputs;
    float anomaly_threshold;
    uint32_t reaction_fast_ticks;
    uint32_t reaction_min_timed;
    float reaction_threshold;
    float foresight_fit_rad;
    float foresight_clear_rad;
    uint32_t foresight_min_foreseen;
    float foresight_threshold;
} AegisConfig;

typedef struct AegisAlert {
    uint32_t tick;     /* of the record that raised it */
    uint8_t player;
    uint8_t reason;    /* AEGIS_REASON_* */
    float value;       /* what was measured */
    float threshold;   /* the line it crossed */
    uint32_t samples;  /* how many samples it was measured over */
} AegisAlert;

typedef struct AegisStats {
    uint32_t accepted;
    uint32_t anomalies;
    uint32_t shots;
    uint32_t hits;
    uint32_t exact;
    uint32_t timed;
    uint32_t fast;
    uint32_t glimpsed;
    uint32_t foreseen;
} AegisStats;

typedef struct AegisMonitor AegisMonitor;

uint32_t aegis_abi_version(void);

int32_t aegis_config_default(AegisConfig *out);
/* hz 0 is AEGIS_ERR_ARG. */
int32_t aegis_config_at_tick_rate(uint32_t hz, AegisConfig *out);
/* On AEGIS_ERR_CONFIG writes "field: problem" into msg (len bytes, cut to
 * fit, NUL-terminated). msg may be NULL. */
int32_t aegis_config_validate(const AegisConfig *cfg, char *msg, size_t len);

/* cfg NULL: the defaults. *out is written only on AEGIS_OK. */
int32_t aegis_monitor_new(const AegisConfig *cfg, AegisMonitor **out);
/* NULL is a no-op. */
void aegis_monitor_free(AegisMonitor *m);

/* Each returns the number of alerts the record raised (>= 0); read them
 * with aegis_monitor_poll. */
int32_t aegis_monitor_accepted(AegisMonitor *m, uint32_t tick, uint8_t player, bool anomaly);
int32_t aegis_monitor_rejected(AegisMonitor *m, uint32_t tick, uint8_t player);
/* aim_err: radians from the aim to the nearest enemy. react: ticks from
 * that enemy coming into sight, first shot of a non-prefire engagement
 * only; negative when not timed. */
int32_t aegis_monitor_shot(AegisMonitor *m, uint32_t tick, uint8_t player, bool hit, float aim_err, int32_t react);
/* ahead: radians to the nearest enemy only a newer snapshot than the
 * claimed one showed. claimed (read only when has_claimed): radians to the
 * nearest enemy the claimed snapshot showed. */
int32_t aegis_monitor_glimpse(AegisMonitor *m, uint32_t tick, uint8_t player, bool has_claimed, float claimed,
                              float ahead);
int32_t aegis_monitor_left(AegisMonitor *m, uint32_t tick, uint8_t player);

/* 1 with *out written, or 0 when no alert is waiting. */
int32_t aegis_monitor_poll(AegisMonitor *m, AegisAlert *out);
/* window false: the whole session; true: the last 100 samples of each kind. */
int32_t aegis_monitor_stats(AegisMonitor *m, uint8_t player, bool window, AegisStats *out);

/* Static, never freed; "unknown" for any other number. */
const char *aegis_reason_label(uint8_t reason);

/* ---- Client: a game client speaking to an Aegis relay or origin -------
 *
 * The engine owns the socket. Protocol:
 *   1. send aegis_client_join;
 *   2. on AEGIS_RX_CHALLENGE send aegis_client_join again (it now carries
 *      the cookie);
 *   3. on AEGIS_RX_JOINED inputs may go;
 *   4. each input names the tick of the snapshot it was chosen on (the one
 *      displayed) — the last 16 received are kept with their proofs.
 */

#define AEGIS_ERR_BUFFER (-6)         /* buf too small; nothing written */
#define AEGIS_ERR_NOT_JOINED (-7)     /* an input before JOINED */
#define AEGIS_ERR_UNKNOWN_TICK (-8)   /* tick not among the last 16 snapshots */
#define AEGIS_ERR_TICK_REGRESSED (-9) /* older tick than an earlier input */
#define AEGIS_ERR_BAD_SEAL (-10)      /* did not open under this client's keys */
#define AEGIS_ERR_MALFORMED (-11)     /* not a server message */

#define AEGIS_CLIENT_KEYS_LEN 80
#define AEGIS_NAME_MAX 32
#define AEGIS_SEND_MAX 160          /* holds any datagram a client sends */
#define AEGIS_TICK_NEWEST 0xFFFFFFFFu

#define AEGIS_RX_JOINED 1
#define AEGIS_RX_CHALLENGE 2
#define AEGIS_RX_SNAPSHOT 3
#define AEGIS_RX_EVENT 4

#define AEGIS_EVENT_HIT 1   /* player hit other for damage */
#define AEGIS_EVENT_DEATH 2 /* player killed by other */
#define AEGIS_EVENT_JOIN 3  /* player */
#define AEGIS_EVENT_LEAVE 4 /* player */

/* What a datagram was; fields are read by kind (see AEGIS_RX_*, AEGIS_EVENT_*). */
typedef struct AegisReceived {
    uint32_t tick;
    uint8_t kind;
    uint8_t event;
    uint8_t player; /* JOINED: this client's id */
    uint8_t other;
    uint8_t damage;
} AegisReceived;

typedef struct AegisPlayer {
    float x;
    float y;
    uint8_t id;
    uint8_t health;
    uint8_t alive; /* nonzero: alive */
} AegisPlayer;

typedef struct AegisClient AegisClient;

/* name: UTF-8, at most AEGIS_NAME_MAX bytes. keys: AEGIS_CLIENT_KEYS_LEN
 * bytes from the game's backend (through a relay), or NULL (straight to an
 * origin, unsealed). */
int32_t aegis_client_new(const char *name, const uint8_t *keys, AegisClient **out);
void aegis_client_free(AegisClient *c);
/* Each writes one datagram into buf and returns its length, or a negative
 * status. */
int32_t aegis_client_join(AegisClient *c, uint8_t *buf, size_t cap);
int32_t aegis_client_input(AegisClient *c, uint32_t tick, float move_x, float move_y, float aim_x, float aim_y,
                           bool shoot, uint8_t *buf, size_t cap);
/* data is not modified. */
int32_t aegis_client_receive(AegisClient *c, const uint8_t *data, size_t len, AegisReceived *out);
/* The id once joined, else AEGIS_ERR_NOT_JOINED. */
int32_t aegis_client_player_id(AegisClient *c);
/* The newest snapshot's players. */
int32_t aegis_client_player_count(AegisClient *c);
int32_t aegis_client_player(AegisClient *c, uint32_t i, AegisPlayer *out);

/* ---- Evidence: what the Aegis server measures, over the engine's world ---
 *
 * Per tick: aegis_evidence_begin_tick with every player where this tick's
 * snapshots show them; aegis_evidence_joined for a player admitted mid-tick;
 * aegis_evidence_shot per shot, in resolution order. Write each shot's
 * evidence to a monitor: aegis_monitor_glimpse if has_glimpse, then, once
 * the hit is known, aegis_monitor_shot if has_aim.
 *
 * `seen` is the tick of the snapshot the input was chosen on. The engine
 * must prove it (a MAC'd per-snapshot proof the client echoes), keep it
 * within the last 16 ticks and never let it go back: an unproven `seen`
 * lets a client pick the picture it is judged in.
 */

#define AEGIS_ERR_BUSY (-12) /* called from inside sees on the same handle; refused */

/* Line of sight from (fx, fy) to a player at (tx, ty): nonzero if it sees.
 * Called only during begin_tick / joined, on the calling thread. Must
 * return normally. Calling back in with the same handle is refused
 * (AEGIS_ERR_BUSY); aegis_evidence_free on it from here is undefined. */
typedef int32_t (*AegisSeesFn)(void *ctx, float fx, float fy, float tx, float ty);

typedef struct AegisShotEvidence {
    bool live;        /* false: no evidence (shooter dead now or in its picture) */
    bool has_aim;     /* write aegis_monitor_shot(aim_err, react) */
    bool has_glimpse; /* write aegis_monitor_glimpse(has_claimed, claimed, ahead) */
    bool has_claimed;
    uint8_t enemy;    /* the nearest enemy in the shooter's picture */
    float aim_err;
    int32_t react;    /* negative: not timed */
    float claimed;
    float ahead;
} AegisShotEvidence;

typedef struct AegisEvidence AegisEvidence;

/* point_blank: the game's hitbox radius in its units (an enemy that close
 * is no evidence of aim); 0 for the lab's. */
int32_t aegis_evidence_new(float point_blank, AegisEvidence **out);
void aegis_evidence_free(AegisEvidence *e);
/* players: all of them, alive or dead, ids unique (else AEGIS_ERR_ARG), in
 * an order that stays the same from tick to tick. health is not read. */
int32_t aegis_evidence_begin_tick(AegisEvidence *e, uint32_t tick, const AegisPlayer *players, size_t n,
                                  AegisSeesFn sees, void *ctx);
int32_t aegis_evidence_joined(AegisEvidence *e, uint32_t tick, const AegisPlayer *players, size_t n,
                              AegisSeesFn sees, void *ctx, uint8_t id);
/* alive: the shooter is alive now. */
int32_t aegis_evidence_shot(AegisEvidence *e, uint32_t tick, uint32_t seen, uint8_t shooter, bool alive, float aim_x,
                            float aim_y, AegisShotEvidence *out);

/* ---- Noise: keys from a relay that is its own backend ------------------
 *
 * Exported only by a library built with feature `noise`. A client that
 * knows only the relay's static public key (shipped with the game, or the
 * server list) gets the AEGIS_CLIENT_KEYS_LEN bytes aegis_client_new takes:
 *   1. aegis_noise_hello(pub, 0), send, aegis_noise_answer: CHALLENGE;
 *   2. free, aegis_noise_hello(pub, cookie), send, answer: WELCOME, keys.
 * A datagram that is not an answer (AEGIS_ERR_MALFORMED) changes nothing.
 * A welcome that does not open (AEGIS_ERR_BAD_SEAL) spends the handle: free
 * it and start again; a spent handle returns AEGIS_ERR_ARG.
 */

#define AEGIS_NOISE_PUBLIC_LEN 32
#define AEGIS_NOISE_HELLO_LEN 72
#define AEGIS_NOISE_CHALLENGE 1 /* *cookie written */
#define AEGIS_NOISE_WELCOME 2   /* keys written; the handle is spent */

typedef struct AegisNoise AegisNoise;

/* cookie 0: none yet. Writes the hello into buf (cap >= AEGIS_NOISE_HELLO_LEN,
 * else AEGIS_ERR_BUFFER) and returns its length; *out only on success. */
int32_t aegis_noise_hello(const uint8_t *relay_public, uint64_t cookie, AegisNoise **out, uint8_t *buf, size_t cap);
/* keys: AEGIS_CLIENT_KEYS_LEN bytes. */
int32_t aegis_noise_answer(AegisNoise *n, const uint8_t *data, size_t len, uint64_t *cookie, uint8_t *keys);
void aegis_noise_free(AegisNoise *n);

#ifdef __cplusplus
}
#endif

#endif /* AEGIS_H */
