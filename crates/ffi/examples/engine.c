/*
 * An engine with its own world, using Aegis on its server: no Aegis sim,
 * no Aegis protocol. Two lanes of open ground with fog at 30 units. In each
 * lane an enemy walks out of the fog every 30 ticks and back. Player 1
 * fires 15 ticks after it appears (500 ms at 30 Hz), a little off; player
 * 2 fires on the very tick it appears, dead on. The engine hands Aegis its
 * players and its line of sight each tick, asks for the evidence of each
 * shot, and feeds it to a monitor.
 *
 * `./engine layout` also prints AegisShotEvidence's size and offsets.
 */
#include <math.h>
#include <stdio.h>
#include <string.h>

#include "aegis.h"

#define SIZE(T) printf("size %s %zu\n", #T, sizeof(T))
#define OFF(T, f) printf("offset %s.%s %zu\n", #T, #f, offsetof(T, f))

#define CHECK(expr, want)                                                                                    \
    do {                                                                                                     \
        int32_t got_ = (expr);                                                                               \
        if (got_ != (want)) {                                                                                \
            fprintf(stderr, "%s:%d: %s = %d, want %d\n", __FILE__, __LINE__, #expr, (int)got_, (int)(want)); \
            return 1;                                                                                        \
        }                                                                                                    \
    } while (0)

static int32_t fog(void *ctx, float fx, float fy, float tx, float ty) {
    float r = *(const float *)ctx;
    return hypotf(tx - fx, ty - fy) < r;
}

/* Fire at `target` from `me`, `off` radians wide; feed the monitor. */
static int shoot(AegisEvidence *ev, AegisMonitor *m, uint32_t tick, const AegisPlayer *me, const AegisPlayer *target,
                 float off) {
    float a = atan2f(target->y - me->y, target->x - me->x) + off;
    AegisShotEvidence s;
    CHECK(aegis_evidence_shot(ev, tick, tick, me->id, me->alive, cosf(a), sinf(a), &s), AEGIS_OK);
    if (s.has_glimpse) {
        CHECK(aegis_monitor_glimpse(m, tick, me->id, s.has_claimed, s.claimed, s.ahead) >= 0, 1);
    }
    if (s.has_aim) {
        bool hit = off < 0.05f; /* the engine resolves the hit; here, a stand-in */
        CHECK(aegis_monitor_shot(m, tick, me->id, hit, s.aim_err, s.react) >= 0, 1);
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "layout") == 0) {
        SIZE(AegisShotEvidence);
        OFF(AegisShotEvidence, live);
        OFF(AegisShotEvidence, has_aim);
        OFF(AegisShotEvidence, has_glimpse);
        OFF(AegisShotEvidence, has_claimed);
        OFF(AegisShotEvidence, enemy);
        OFF(AegisShotEvidence, aim_err);
        OFF(AegisShotEvidence, react);
        OFF(AegisShotEvidence, claimed);
        OFF(AegisShotEvidence, ahead);
    }
    float radius = 30.0f;
    AegisEvidence *ev = NULL;
    AegisMonitor *m = NULL;
    CHECK(aegis_evidence_new(0.0f, &ev), AEGIS_OK);
    CHECK(aegis_monitor_new(NULL, &m), AEGIS_OK);

    /* Lane 1 at y = 0, lane 2 at y = 200: nobody sees across. */
    AegisPlayer world[4] = {
        {0.0f, 0.0f, 1, 100, true},
        {0.0f, 200.0f, 2, 100, true},
        {50.0f, 0.0f, 3, 100, true},
        {50.0f, 200.0f, 4, 100, true},
    };
    int timed = 0;
    for (uint32_t tick = 1; tick <= 900; tick++) {
        /* The enemies: in sight (x = 20) for ticks 10..29 of every 30. */
        float x = (tick % 30) >= 10 ? 20.0f : 50.0f;
        world[2].x = x;
        world[3].x = x;
        CHECK(aegis_evidence_begin_tick(ev, tick, world, 4, fog, &radius), AEGIS_OK);
        if (tick % 30 == 10) {
            if (shoot(ev, m, tick, &world[1], &world[3], 0.0f)) return 1;
            timed++;
        }
        if (tick % 30 == 25) {
            if (shoot(ev, m, tick, &world[0], &world[2], 0.1f)) return 1;
        }
    }

    AegisAlert a;
    int flagged_1 = 0, flagged_2 = 0;
    while (aegis_monitor_poll(m, &a) == 1) {
        printf("alert tick=%u player=%u %s value=%.3f over %u\n", a.tick, a.player, aegis_reason_label(a.reason),
               a.value, a.samples);
        flagged_1 += a.player == 1;
        flagged_2 += a.player == 2 && a.reason == AEGIS_REASON_REACTION;
    }
    AegisStats s;
    CHECK(aegis_monitor_stats(m, 1, false, &s), AEGIS_OK);
    printf("player 1: %u shots, %u timed, %u fast\n", s.shots, s.timed, s.fast);
    CHECK(aegis_monitor_stats(m, 2, false, &s), AEGIS_OK);
    printf("player 2: %u shots, %u timed, %u fast\n", s.shots, s.timed, s.fast);
    CHECK((int32_t)s.timed, timed);
    aegis_monitor_free(m);
    aegis_evidence_free(ev);
    if (flagged_1 != 0 || flagged_2 == 0) {
        fprintf(stderr, "player 1 flagged %d times, player 2 by reaction %d times\n", flagged_1, flagged_2);
        return 1;
    }
    printf("ok\n");
    return 0;
}
