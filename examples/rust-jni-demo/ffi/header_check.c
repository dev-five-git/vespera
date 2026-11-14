/*
 * Compile check for crates/vespera_inprocess/include/vespera.h: CI compiles
 * this file (syntax only, nothing is linked) as strict C99 and as C++ with
 * every warning an error, so the header stays valid for both languages.
 *
 *   cc  -std=c99 -Wall -Wextra -pedantic -Werror -fsyntax-only -I crates/vespera_inprocess/include examples/rust-jni-demo/ffi/header_check.c
 *   c++ -x c++ -Wall -Wextra -pedantic -Werror -fsyntax-only -I crates/vespera_inprocess/include examples/rust-jni-demo/ffi/header_check.c
 */
#include "vespera.h"

static void on_response(void *user_data, uint8_t *response, size_t response_len) {
    (void)user_data;
    vespera_free(response, response_len);
}

int main(void) {
    /* Every prototype, taken as a pointer of the type its C signature names. */
    uint32_t (*abi_version)(void) = vespera_abi_version;
    int (*init)(void) = vespera_init;
    int (*dispatch)(const uint8_t *, size_t, uint8_t **, size_t *) = vespera_dispatch;
    int (*dispatch_into)(const uint8_t *, size_t, uint8_t *, size_t, size_t *) = vespera_dispatch_into;
    int (*dispatch_async)(const uint8_t *, size_t, vespera_callback, void *) = vespera_dispatch_async;
    void (*release)(uint8_t *, size_t) = vespera_free;
    int (*shutdown)(uint64_t) = vespera_shutdown;
    int (*sized[5])(size_t) = {
        vespera_configure_worker_threads,
        vespera_configure_max_blocking_threads,
        vespera_configure_max_request_bytes,
        vespera_configure_max_buffered_response_bytes,
        vespera_configure_async_max_in_flight,
    };
    int (*timeout)(uint64_t) = vespera_configure_dispatch_timeout_ms;
    vespera_callback callback = on_response;
    int codes[] = {
        VESPERA_OK,           VESPERA_OVERFLOW,         VESPERA_ERR_INVALID_ARGUMENT,
        VESPERA_ERR_PANIC,    VESPERA_ERR_REGISTRATION, VESPERA_ERR_RUNTIME,
        VESPERA_ERR_TOO_LATE,
    };
    (void)abi_version;
    (void)init;
    (void)dispatch;
    (void)dispatch_into;
    (void)dispatch_async;
    (void)release;
    (void)shutdown;
    (void)sized;
    (void)timeout;
    (void)callback;
    (void)codes;
    return VESPERA_ABI_VERSION == 1 ? 0 : 1;
}
