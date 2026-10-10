/*
 * The Aegis detector from C: a 60 Hz game feeds one honest player and one
 * that fires 0 ticks after every enemy appears, and prints the alerts.
 *
 * Build (Linux), from the workspace root:
 *   cargo build -p aegis-ffi
 *   cc crates/ffi/examples/monitor.c -Icrates/ffi/include -Ltarget/debug -laegis_ffi -o monitor
 *   LD_LIBRARY_PATH=target/debug ./monitor
 *
 * `./monitor layout` also prints every struct's size and field offset, which
 * the crate's test compares with Rust's.
 */
#include <stdio.h>
#include <string.h>

#include "aegis.h"

#define SIZE(T) printf("size %s %zu\n", #T, sizeof(T))
#define OFF(T, f) printf("offset %s.%s %zu\n", #T, #f, offsetof(T, f))

static void layout(void) {
    SIZE(AegisConfig);
    OFF(AegisConfig, accuracy_min_shots);
    OFF(AegisConfig, accuracy_threshold);
    OFF(AegisConfig, aim_exact_rad);
    OFF(AegisConfig, aim_exact_min_shots);
    OFF(AegisConfig, aim_exact_threshold);
    OFF(AegisConfig, anomaly_min_inputs);
    OFF(AegisConfig, anomaly_threshold);
    OFF(AegisConfig, reaction_fast_ticks);
    OFF(AegisConfig, reaction_min_timed);
    OFF(AegisConfig, reaction_threshold);
    OFF(AegisConfig, foresight_fit_rad);
    OFF(AegisConfig, foresight_clear_rad);
    OFF(AegisConfig, foresight_min_foreseen);
    OFF(AegisConfig, foresight_threshold);
    SIZE(AegisAlert);
    OFF(AegisAlert, tick);
    OFF(AegisAlert, player);
    OFF(AegisAlert, reason);
    OFF(AegisAlert, value);
    OFF(AegisAlert, threshold);
    OFF(AegisAlert, samples);
    SIZE(AegisStats);
    OFF(AegisStats, accepted);
    OFF(AegisStats, anomalies);
    OFF(AegisStats, shots);
    OFF(AegisStats, hits);
    OFF(AegisStats, exact);
    OFF(AegisStats, timed);
    OFF(AegisStats, fast);
    OFF(AegisStats, glimpsed);
    OFF(AegisStats, foreseen);
}

#define CHECK(expr, want)                                                                                    \
    do {                                                                                                     \
        int32_t got_ = (expr);                                                                               \
        if (got_ != (want)) {                                                                                \
            fprintf(stderr, "%s:%d: %s = %d, want %d\n", __FILE__, __LINE__, #expr, (int)got_, (int)(want)); \
            return 1;                                                                                        \
        }                                                                                                    \
    } while (0)

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "layout") == 0) {
        layout();
    }
    if (aegis_abi_version() != AEGIS_ABI_VERSION) {
        fprintf(stderr, "header is ABI %u, library is %u\n", AEGIS_ABI_VERSION, aegis_abi_version());
        return 1;
    }

    /* A config that cannot mean anything is refused, with its reason. */
    AegisConfig cfg;
    char why[96];
    CHECK(aegis_config_at_tick_rate(60, &cfg), AEGIS_OK);
    AegisConfig bad = cfg;
    bad.foresight_clear_rad = bad.foresight_fit_rad;
    CHECK(aegis_config_validate(&bad, why, sizeof why), AEGIS_ERR_CONFIG);
    printf("refused: %s\n", why);
    AegisMonitor *m = NULL;
    CHECK(aegis_monitor_new(&bad, &m), AEGIS_ERR_CONFIG);

    CHECK(aegis_monitor_new(&cfg, &m), AEGIS_OK);
    printf("fast at 60 Hz: <= %u ticks\n", cfg.reaction_fast_ticks);

    /* Player 1 reacts in 15 ticks (250 ms) and hits half; player 2 fires
     * the tick the enemy appears and never misses. One engagement every
     * 30 ticks, each with an accepted input. */
    for (uint32_t t = 0; t < 600; t++) {
        CHECK(aegis_monitor_accepted(m, t, 1, false) >= 0, 1);
        CHECK(aegis_monitor_accepted(m, t, 2, false) >= 0, 1);
        if (t % 30 == 15) {
            CHECK(aegis_monitor_shot(m, t, 1, t % 60 == 15, 0.12f, 15) >= 0, 1);
            CHECK(aegis_monitor_shot(m, t, 2, true, 0.002f, 0) >= 0, 1);
        }
    }

    AegisAlert a;
    int flagged_1 = 0, flagged_2 = 0;
    int32_t got;
    while ((got = aegis_monitor_poll(m, &a)) == 1) {
        printf("alert tick=%u player=%u %s value=%.3f line=%.3f over %u\n", a.tick, a.player,
               aegis_reason_label(a.reason), a.value, a.threshold, a.samples);
        flagged_1 += a.player == 1;
        flagged_2 += a.player == 2;
    }
    CHECK(got, 0);

    /* The rest of the ABI, so every declared function is linked. */
    AegisConfig lab;
    CHECK(aegis_config_default(&lab), AEGIS_OK);
    CHECK(aegis_monitor_rejected(m, 600, 1), 0);
    CHECK(aegis_monitor_glimpse(m, 600, 1, true, 0.05f, 0.0f), 0);

    AegisStats s;
    CHECK(aegis_monitor_stats(m, 1, true, &s), AEGIS_OK);
    CHECK((int32_t)s.glimpsed, 1);
    CHECK(aegis_monitor_stats(m, 2, false, &s), AEGIS_OK);
    printf("player 2: %u shots, %u timed, %u fast\n", s.shots, s.timed, s.fast);
    CHECK(aegis_monitor_left(m, 600, 2), 0);
    CHECK(aegis_monitor_stats(m, 2, false, &s), AEGIS_ERR_NO_SESSION);
    aegis_monitor_free(m);

    if (flagged_1 != 0 || flagged_2 == 0) {
        fprintf(stderr, "honest flagged %d times, cheat %d times\n", flagged_1, flagged_2);
        return 1;
    }
    printf("ok\n");
    return 0;
}
