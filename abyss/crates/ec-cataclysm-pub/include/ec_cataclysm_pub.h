/* SPDX-License-Identifier: Apache-2.0 */
/*
 * ec_cataclysm_pub.h — C ABI of ec-cataclysm-pub (ADR 0034, P-04 §7,
 * COMP-16 milestone 21).
 *
 * The semantic publisher for the `cataclysm` terminal. The emulator feeds it
 * grid state; it decides when to publish and returns the eclipse_semantic_v1
 * requests (COMP-09 §2) to send, in order, ending in `commit`. The emulator
 * forwards each op to the matching request and makes no protocol decision
 * of its own (ADR 0034: nothing protocol-shaped is reimplemented in C).
 *
 * Hand-written; it mirrors src/ffi.rs and src/semantic.rs. The FFI fixture
 * (tests/c/fixtures.c) compiles against this file and checks
 * ecpub_op_size() == sizeof(ecpub_op).
 *
 * ---------------------------------------------------------------------------
 * OWNERSHIP AND LIFETIME
 * ---------------------------------------------------------------------------
 *
 * ecpub_publisher
 *     Created by ecpub_new, owned by the caller, released by exactly one
 *     ecpub_free. Never free it any other way; never use it after
 *     ecpub_free. ecpub_free(NULL) is a no-op. Not thread-safe: one thread
 *     at a time (the emulator's event loop). No global state, so separate
 *     publishers may live on separate threads.
 *
 * ecpub_config
 *     Caller-owned, read during ecpub_new only; not retained. NULL means
 *     defaults. Every field is a floor: a value below the default is raised
 *     to it (a caller may slow publishing, never speed it up, and may
 *     lengthen downgrade_grace, never shorten it).
 *
 * Input buffers (ecpub_set_line text, ecpub_osc133 payload)
 *     Caller-owned, borrowed for the duration of the call only, copied
 *     before return. Not required to be NUL-terminated or valid UTF-8; may
 *     be NULL only when len == 0.
 *
 * ecpub_batch and everything it points to
 *     Owned by the publisher. `ops`, and every `key`/`str` inside them, stay
 *     valid until the next call that takes the publisher non-const
 *     (any ecpub_* call except ecpub_next_deadline, ecpub_get_stats,
 *     ecpub_abi_version, ecpub_op_size) or ecpub_free, whichever is first.
 *     Never free or write through them. Copy anything needed later. The
 *     intended use is: poll, forward every op to the wire, then continue.
 *
 * Strings in ops (`key`, `str`)
 *     UTF-8, NUL-terminated, contain no interior NUL, and `*_len` excludes
 *     the terminator; both are NULL with length 0 when the op has none. They
 *     can be passed straight to wl_proxy_marshal string arguments.
 *
 * ---------------------------------------------------------------------------
 * CALLING PATTERN
 * ---------------------------------------------------------------------------
 *
 *   after each read from the pty (or batch of grid changes):
 *       ecpub_set_line / ecpub_scroll / ecpub_set_cursor / ecpub_osc133 ...
 *       on any pty termios change: ecpub_set_echo(p, ECHO is set, now)
 *       ecpub_poll(p, now, &b); forward b.ops[0..b.len)
 *   arm a timer for ecpub_next_deadline(p) and ecpub_poll when it fires.
 *
 * `now_ns` is CLOCK_MONOTONIC nanoseconds, non-decreasing across calls.
 * Output time is taken at poll, so poll after every batch of input.
 *
 * Echo-off (P-04 §6.1): after ecpub_set_echo(p, false, ...) the next
 * ecpub_poll returns a batch flagged ECPUB_BATCH_URGENT holding only
 * set_ext(root, "echo", "false") + commit, ahead of every rate limit. Send it
 * before reading the pty again. Until echo has been back on for
 * downgrade_grace, no poll returns anything.
 */
#ifndef EC_CATACLYSM_PUB_H
#define EC_CATACLYSM_PUB_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define ECPUB_ABI_VERSION 1u

/* Return codes. */
#define ECPUB_OK 0
#define ECPUB_ERR_NULL (-1)  /* a required pointer was NULL */
#define ECPUB_ERR_RANGE (-2) /* row/col/size out of range; nothing changed */

/* Limits. */
#define ECPUB_MAX_ROWS 1024u
#define ECPUB_MAX_COLS 4096u
#define ECPUB_MAX_LINE_BYTES 3072u /* line text cut on a char boundary */
#define ECPUB_EXT_VALUE_MAX 256u   /* COMP-09 §7.3 */

/* Node ids (COMP-09 `node: uint`). */
#define ECPUB_ROOT_ID 1u            /* role terminal */
#define ECPUB_SLOT_BASE 0x100u      /* screen row r = ECPUB_SLOT_BASE + r */
#define ECPUB_GROUP_BASE 0x10000u   /* command blocks; never reused */

/* Op kinds: one per eclipse_semantic_v1 request (COMP-09 §2). */
#define ECPUB_OP_SET_ROOT 1u       /* node */
#define ECPUB_OP_ADD_NODE 2u       /* node, parent, index */
#define ECPUB_OP_REMOVE_NODE 3u    /* node (subtree) */
#define ECPUB_OP_MOVE_NODE 4u      /* node, parent, index */
#define ECPUB_OP_SET_ROLE 5u       /* node, role */
#define ECPUB_OP_SET_VALUE_TEXT 6u /* node, str, cursor, sel_start, sel_end */
#define ECPUB_OP_SET_EXT 7u        /* node, key, str */
#define ECPUB_OP_COMMIT 8u         /* always the last op */

/* Role codes: P-01 §1.1 listing order. Only these are emitted. */
#define ECPUB_ROLE_GROUP 10u
#define ECPUB_ROLE_TERMINAL 48u
#define ECPUB_ROLE_TERMINAL_LINE 49u

/* Batch flags. */
#define ECPUB_BATCH_STRUCTURAL (1u << 0) /* adds/removes/moves a node */
#define ECPUB_BATCH_URGENT (1u << 1)     /* echo-off raise; send at once */

typedef struct ecpub_publisher ecpub_publisher;

typedef struct ecpub_config {
    uint64_t min_interval_ns;      /* output-driven spacing, default 200 ms */
    uint64_t boundary_interval_ns; /* boundary-driven spacing, default 100 ms */
    uint64_t idle_ns;              /* quiet time counted as idle, default 100 ms */
    uint64_t downgrade_grace_ns;   /* S-05 §5.2, default 500 ms */
} ecpub_config;

typedef struct ecpub_op {
    uint32_t kind;
    uint32_t node;
    uint32_t parent;
    uint32_t index;
    uint32_t role;
    int32_t cursor; /* set_value_text: cursor column, -1 if not on this line */
    int32_t sel_start;
    int32_t sel_end;
    const char *key; /* set_ext key, bare (no "ext." prefix) */
    size_t key_len;
    const char *str; /* set_value_text text / set_ext value */
    size_t str_len;
} ecpub_op;

typedef struct ecpub_batch {
    const ecpub_op *ops;
    size_t len;
    uint64_t seq; /* 1, 2, 3, ... */
    uint32_t flags;
} ecpub_batch;

typedef struct ecpub_stats {
    uint64_t retained_bytes; /* heap held by the publisher */
    uint64_t publishes;
    uint64_t structural_publishes;
    uint64_t blocks; /* live command blocks */
} ecpub_stats;

uint32_t ecpub_abi_version(void);
size_t ecpub_op_size(void);

/* Fill *out with the defaults. NULL is ignored. */
void ecpub_config_default(ecpub_config *out);

/* NULL if rows/cols is 0 or above the limits. cfg may be NULL. */
ecpub_publisher *ecpub_new(const ecpub_config *cfg, uint16_t rows, uint16_t cols);
void ecpub_free(ecpub_publisher *p);

int ecpub_resize(ecpub_publisher *p, uint16_t rows, uint16_t cols);
/* Replace screen row `row` of the active screen. */
int ecpub_set_line(ecpub_publisher *p, uint16_t row, const char *utf8, size_t len);
/* Top n rows of the active screen scrolled off; bottom n are now empty. */
int ecpub_scroll(ecpub_publisher *p, uint16_t n);
int ecpub_set_cursor(ecpub_publisher *p, uint16_t row, uint16_t col);
int ecpub_set_alt_screen(ecpub_publisher *p, bool on);
/* The pty's termios ECHO flag. */
int ecpub_set_echo(ecpub_publisher *p, bool on, uint64_t now_ns);
/* OSC 133 body ("A", "D;0", or "133;D;0"). Unknown payloads are ignored. */
int ecpub_osc133(ecpub_publisher *p, const char *payload, size_t len, uint64_t now_ns);

/* 1: *out holds a batch. 0: nothing due (*out zeroed). <0: error. */
int ecpub_poll(ecpub_publisher *p, uint64_t now_ns, ecpub_batch *out);
/* Next time a poll may publish; 0 = now, UINT64_MAX = nothing pending. */
uint64_t ecpub_next_deadline(const ecpub_publisher *p);
int ecpub_get_stats(const ecpub_publisher *p, ecpub_stats *out);

#ifdef __cplusplus
}
#endif

#endif /* EC_CATACLYSM_PUB_H */
