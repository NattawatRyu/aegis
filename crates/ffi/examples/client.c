/*
 * The Aegis client from C, without a socket: what an engine's client does
 * with the bytes it sends and receives. The full round trip through a real
 * relay and origin is the crate's test `through_relay`.
 *
 * `./client layout` also prints every client struct's size and field
 * offset and every constant in aegis.h, which the crate's test compares
 * with Rust's.
 */
#include <stdio.h>
#include <string.h>

#include "aegis.h"

#define SIZE(T) printf("size %s %zu\n", #T, sizeof(T))
#define OFF(T, f) printf("offset %s.%s %zu\n", #T, #f, offsetof(T, f))
#define CONST(c) printf("const %s %lld\n", #c, (long long)(c))

static void layout(void) {
    SIZE(AegisReceived);
    OFF(AegisReceived, tick);
    OFF(AegisReceived, kind);
    OFF(AegisReceived, event);
    OFF(AegisReceived, player);
    OFF(AegisReceived, other);
    OFF(AegisReceived, damage);
    SIZE(AegisPlayer);
    OFF(AegisPlayer, x);
    OFF(AegisPlayer, y);
    OFF(AegisPlayer, id);
    OFF(AegisPlayer, health);
    OFF(AegisPlayer, alive);
    CONST(AEGIS_ABI_VERSION);
    CONST(AEGIS_OK);
    CONST(AEGIS_ERR_NULL);
    CONST(AEGIS_ERR_CONFIG);
    CONST(AEGIS_ERR_PANIC);
    CONST(AEGIS_ERR_NO_SESSION);
    CONST(AEGIS_ERR_ARG);
    CONST(AEGIS_ERR_BUFFER);
    CONST(AEGIS_ERR_NOT_JOINED);
    CONST(AEGIS_ERR_UNKNOWN_TICK);
    CONST(AEGIS_ERR_TICK_REGRESSED);
    CONST(AEGIS_ERR_BAD_SEAL);
    CONST(AEGIS_ERR_MALFORMED);
    CONST(AEGIS_ERR_BUSY);
    CONST(AEGIS_REASON_ACCURACY);
    CONST(AEGIS_REASON_AIM_EXACT);
    CONST(AEGIS_REASON_ANOMALY_RATE);
    CONST(AEGIS_REASON_REACTION);
    CONST(AEGIS_REASON_FORESIGHT);
    CONST(AEGIS_REASON_FAR_AIM);
    CONST(AEGIS_CLIENT_KEYS_LEN);
    CONST(AEGIS_NAME_MAX);
    CONST(AEGIS_SEND_MAX);
    CONST(AEGIS_TICK_NEWEST);
    CONST(AEGIS_RX_JOINED);
    CONST(AEGIS_RX_CHALLENGE);
    CONST(AEGIS_RX_SNAPSHOT);
    CONST(AEGIS_RX_EVENT);
    CONST(AEGIS_EVENT_HIT);
    CONST(AEGIS_EVENT_DEATH);
    CONST(AEGIS_EVENT_JOIN);
    CONST(AEGIS_EVENT_LEAVE);
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
    AegisClient *c = NULL;
    CHECK(aegis_client_new("a name longer than thirty-two bytes", NULL, &c), AEGIS_ERR_ARG);
    CHECK(aegis_client_new("riw", NULL, &c), AEGIS_OK);

    uint8_t buf[AEGIS_SEND_MAX];
    int32_t n = aegis_client_join(c, buf, sizeof buf);
    printf("join: %d bytes\n", (int)n);
    CHECK(n > 0, 1);
    CHECK(aegis_client_join(c, buf, (size_t)n - 1), AEGIS_ERR_BUFFER);
    CHECK(aegis_client_input(c, AEGIS_TICK_NEWEST, 1, 0, 1, 0, true, buf, sizeof buf), AEGIS_ERR_NOT_JOINED);
    CHECK(aegis_client_player_id(c), AEGIS_ERR_NOT_JOINED);
    CHECK(aegis_client_player_count(c), 0);
    AegisPlayer p;
    CHECK(aegis_client_player(c, 0, &p), AEGIS_ERR_ARG);

    AegisReceived rx;
    const uint8_t garbage[] = {0xff, 0xff, 0xff, 0xff, 1, 2, 3};
    CHECK(aegis_client_receive(c, garbage, sizeof garbage, &rx), AEGIS_ERR_MALFORMED);
    aegis_client_free(c);

    /* Sealed: nothing unsealed opens. */
    uint8_t keys[AEGIS_CLIENT_KEYS_LEN] = {0};
    CHECK(aegis_client_new("riw", keys, &c), AEGIS_OK);
    n = aegis_client_join(c, buf, sizeof buf);
    printf("sealed join: %d bytes\n", (int)n);
    CHECK(aegis_client_receive(c, garbage, sizeof garbage, &rx), AEGIS_ERR_BAD_SEAL);
    aegis_client_free(c);

    printf("ok\n");
    return 0;
}
