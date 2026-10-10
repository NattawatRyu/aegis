/*
 * The Noise handshake from C, without a socket (library built with feature
 * `noise`): a hello, a stray datagram that changes nothing, the relay's
 * cookie challenge, and a hello carrying the cookie. The full handshake
 * against a real relay, and the client it hands keys to playing through
 * it, is the crate's test `through_relay`.
 *
 * `./noise layout` also prints the Noise constants in aegis.h.
 */
#include <stdio.h>
#include <string.h>

#include "aegis.h"

#define CONST(c) printf("const %s %lld\n", #c, (long long)(c))

#define CHECK(expr, want)                                                                                    \
    do {                                                                                                     \
        int32_t got_ = (expr);                                                                               \
        if (got_ != (want)) {                                                                                \
            fprintf(stderr, "%s:%d: %s = %d, want %d\n", __FILE__, __LINE__, #expr, (int)got_, (int)(want)); \
            return 1;                                                                                        \
        }                                                                                                    \
    } while (0)

/* Every answer starts as the hello does: the 16-byte hello session id. */
#define SID_LEN 16

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "layout") == 0) {
        CONST(AEGIS_NOISE_PUBLIC_LEN);
        CONST(AEGIS_NOISE_HELLO_LEN);
        CONST(AEGIS_NOISE_CHALLENGE);
        CONST(AEGIS_NOISE_WELCOME);
    }
    uint8_t relay_public[AEGIS_NOISE_PUBLIC_LEN];
    for (int i = 0; i < AEGIS_NOISE_PUBLIC_LEN; i++) relay_public[i] = (uint8_t)(9 + i);

    uint8_t hello[AEGIS_NOISE_HELLO_LEN];
    AegisNoise *n = NULL;
    CHECK(aegis_noise_hello(relay_public, 0, &n, hello, sizeof hello - 1), AEGIS_ERR_BUFFER);
    CHECK(n == NULL, 1);
    CHECK(aegis_noise_hello(relay_public, 0, &n, hello, sizeof hello), AEGIS_NOISE_HELLO_LEN);

    uint64_t cookie = 0;
    uint8_t keys[AEGIS_CLIENT_KEYS_LEN];
    uint8_t stray[3] = {1, 2, 3};
    CHECK(aegis_noise_answer(n, stray, sizeof stray, &cookie, keys), AEGIS_ERR_MALFORMED);
    CHECK(aegis_noise_answer(n, stray, sizeof stray, NULL, keys), AEGIS_ERR_NULL);

    /* The relay's challenge: the hello's session id, then the cookie (LE). */
    uint8_t challenge[SID_LEN + 8];
    memcpy(challenge, hello, SID_LEN);
    uint64_t want = 0x0123456789abcdefull;
    for (int i = 0; i < 8; i++) challenge[SID_LEN + i] = (uint8_t)(want >> (8 * i));
    CHECK(aegis_noise_answer(n, challenge, sizeof challenge, &cookie, keys), AEGIS_NOISE_CHALLENGE);
    CHECK(cookie == want, 1);
    aegis_noise_free(n);

    CHECK(aegis_noise_hello(relay_public, cookie, &n, hello, sizeof hello), AEGIS_NOISE_HELLO_LEN);
    uint64_t sent = 0;
    for (int i = 0; i < 8; i++) sent |= (uint64_t)hello[SID_LEN + i] << (8 * i);
    CHECK(sent == cookie, 1);
    aegis_noise_free(n);
    aegis_noise_free(NULL);
    printf("ok\n");
    return 0;
}
