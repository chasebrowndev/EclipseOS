/* SPDX-License-Identifier: Apache-2.0 */
/*
 * P-04 §8 fixtures run through the C ABI exactly as `cataclysm` links it
 * (ADR 0034, COMP-16 milestone 21): throughput, echo-off, OSC 133, plus the
 * ownership/lifetime rules of ec_cataclysm_pub.h. Built and run under
 * AddressSanitizer + LeakSanitizer by tests/ffi.rs.
 *
 * Every batch is applied to a mirror tree that enforces COMP-09 request
 * validity (no add of a live id, no move into a descendant, parents exist,
 * commit last, strings NUL-terminated and within limits). The mirror is a
 * test oracle, not a second publisher.
 */
#include "ec_cataclysm_pub.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(c)                                                                \
    do {                                                                        \
        if (!(c)) {                                                             \
            fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #c); \
            exit(1);                                                            \
        }                                                                       \
    } while (0)

#define MS 1000000ull

/* ------------------------------------------------------------------------ */
/* Mirror tree                                                              */
/* ------------------------------------------------------------------------ */

#define MAXN 600
#define MAXEXT 16
#define MAXCH 1100

typedef struct {
    char key[32];
    char val[ECPUB_EXT_VALUE_MAX + 1];
} ext_t;

typedef struct {
    uint32_t id;
    int live;
    uint32_t parent;
    uint32_t role;
    char text[ECPUB_MAX_LINE_BYTES + 1];
    int32_t cursor;
    ext_t ext[MAXEXT];
    int next;
    uint32_t children[MAXCH];
    int nchild;
} node_t;

typedef struct {
    node_t n[MAXN];
    uint32_t root;
    uint64_t batches, structural, urgent, value_ops, ext_ops, tree_ops;
} mirror_t;

static node_t *find(mirror_t *m, uint32_t id) {
    for (int i = 0; i < MAXN; i++)
        if (m->n[i].live && m->n[i].id == id) return &m->n[i];
    return NULL;
}

static node_t *create(mirror_t *m, uint32_t id) {
    CHECK(find(m, id) == NULL);
    for (int i = 0; i < MAXN; i++) {
        if (!m->n[i].live) {
            memset(&m->n[i], 0, sizeof m->n[i]);
            m->n[i].id = id;
            m->n[i].live = 1;
            m->n[i].cursor = -1;
            return &m->n[i];
        }
    }
    CHECK(!"mirror full");
    return NULL;
}

static void detach(mirror_t *m, node_t *n) {
    node_t *p = n->parent ? find(m, n->parent) : NULL;
    if (!p) return;
    for (int i = 0; i < p->nchild; i++) {
        if (p->children[i] == n->id) {
            memmove(&p->children[i], &p->children[i + 1], (size_t)(p->nchild - i - 1) * sizeof(uint32_t));
            p->nchild--;
            break;
        }
    }
    n->parent = 0;
}

static void attach(node_t *n, node_t *p, uint32_t index) {
    CHECK(p->nchild < MAXCH);
    int at = index > (uint32_t)p->nchild ? p->nchild : (int)index;
    memmove(&p->children[at + 1], &p->children[at], (size_t)(p->nchild - at) * sizeof(uint32_t));
    p->children[at] = n->id;
    p->nchild++;
    n->parent = p->id;
}

static void remove_subtree(mirror_t *m, node_t *n) {
    while (n->nchild > 0) {
        node_t *c = find(m, n->children[n->nchild - 1]);
        CHECK(c);
        n->nchild--;
        c->parent = 0;
        remove_subtree(m, c);
    }
    n->live = 0;
}

static int is_self_or_ancestor(mirror_t *m, node_t *a, node_t *b) {
    for (node_t *x = b; x; x = x->parent ? find(m, x->parent) : NULL)
        if (x == a) return 1;
    return 0;
}

static int valid_utf8_no_ctrl(const char *s, size_t len) {
    const unsigned char *u = (const unsigned char *)s;
    for (size_t i = 0; i < len;) {
        unsigned c = u[i];
        size_t n = c < 0x80 ? 1 : (c >> 5) == 6 ? 2 : (c >> 4) == 14 ? 3 : (c >> 3) == 30 ? 4 : 0;
        if (n == 0 || i + n > len) return 0;
        if (n == 1 && (c < 0x20 || c == 0x7f)) return 0;
        for (size_t k = 1; k < n; k++)
            if ((u[i + k] & 0xc0) != 0x80) return 0;
        if (n == 2 && c == 0xc2 && u[i + 1] < 0xa0) return 0; /* C1 controls */
        i += n;
    }
    return 1;
}

static void check_str(const char *s, size_t len, size_t max) {
    CHECK(s != NULL);
    CHECK(strlen(s) == len);
    CHECK(len <= max);
    CHECK(valid_utf8_no_ctrl(s, len));
}

static void apply(mirror_t *m, const ecpub_batch *b) {
    CHECK(b->ops != NULL && b->len >= 2);
    CHECK(b->ops[b->len - 1].kind == ECPUB_OP_COMMIT);
    m->batches++;
    if (b->flags & ECPUB_BATCH_STRUCTURAL) m->structural++;
    if (b->flags & ECPUB_BATCH_URGENT) m->urgent++;
    int saw_tree = 0;
    for (size_t i = 0; i < b->len; i++) {
        const ecpub_op *op = &b->ops[i];
        node_t *n, *p;
        if (op->kind != ECPUB_OP_SET_EXT) CHECK(op->key == NULL && op->key_len == 0);
        switch (op->kind) {
        case ECPUB_OP_SET_ROOT:
            CHECK(m->root == 0);
            create(m, op->node);
            m->root = op->node;
            saw_tree = 1;
            break;
        case ECPUB_OP_ADD_NODE:
            p = find(m, op->parent);
            CHECK(p);
            n = create(m, op->node);
            attach(n, p, op->index);
            saw_tree = 1;
            m->tree_ops++;
            break;
        case ECPUB_OP_MOVE_NODE:
            n = find(m, op->node);
            p = find(m, op->parent);
            CHECK(n && p && n->id != m->root);
            CHECK(!is_self_or_ancestor(m, n, p));
            detach(m, n);
            attach(n, p, op->index);
            saw_tree = 1;
            m->tree_ops++;
            break;
        case ECPUB_OP_REMOVE_NODE:
            n = find(m, op->node);
            CHECK(n && n->id != m->root);
            detach(m, n);
            remove_subtree(m, n);
            saw_tree = 1;
            m->tree_ops++;
            break;
        case ECPUB_OP_SET_ROLE:
            n = find(m, op->node);
            CHECK(n);
            n->role = op->role;
            break;
        case ECPUB_OP_SET_VALUE_TEXT:
            n = find(m, op->node);
            CHECK(n && n->role == ECPUB_ROLE_TERMINAL_LINE);
            check_str(op->str, op->str_len, ECPUB_MAX_LINE_BYTES);
            memcpy(n->text, op->str, op->str_len + 1);
            n->cursor = op->cursor;
            CHECK(op->sel_start == -1 && op->sel_end == -1);
            m->value_ops++;
            break;
        case ECPUB_OP_SET_EXT: {
            n = find(m, op->node);
            CHECK(n);
            check_str(op->key, op->key_len, 31);
            check_str(op->str, op->str_len, ECPUB_EXT_VALUE_MAX);
            CHECK(strncmp(op->key, "ext.", 4) != 0);
            int k;
            for (k = 0; k < n->next; k++)
                if (strcmp(n->ext[k].key, op->key) == 0) break;
            if (k == n->next) {
                CHECK(n->next < MAXEXT); /* COMP-09 §7.3: 16 keys */
                n->next++;
            }
            memcpy(n->ext[k].key, op->key, op->key_len + 1);
            memcpy(n->ext[k].val, op->str, op->str_len + 1);
            m->ext_ops++;
            break;
        }
        case ECPUB_OP_COMMIT:
            CHECK(i == b->len - 1);
            break;
        default:
            CHECK(!"unknown op kind");
        }
    }
    CHECK(!!(b->flags & ECPUB_BATCH_STRUCTURAL) == saw_tree);
}

static const char *ext(mirror_t *m, uint32_t id, const char *key) {
    node_t *n = find(m, id);
    if (!n) return NULL;
    for (int k = 0; k < n->next; k++)
        if (strcmp(n->ext[k].key, key) == 0) return n->ext[k].val;
    return NULL;
}

static mirror_t *mirror_new(void) {
    mirror_t *m = calloc(1, sizeof *m);
    CHECK(m);
    return m;
}

/* ------------------------------------------------------------------------ */
/* A toy emulator: what foot does to the grid, reduced to lines.            */
/* ------------------------------------------------------------------------ */

#define TROWS 64
#define TCOLS 512

typedef struct {
    ecpub_publisher *p;
    mirror_t *m;
    int rows, cols, crow, ccol;
    char lines[TROWS][TCOLS];
} term_t;

static void term_init(term_t *t, int rows, int cols) {
    memset(t, 0, sizeof *t);
    t->rows = rows;
    t->cols = cols;
    t->p = ecpub_new(NULL, (uint16_t)rows, (uint16_t)cols);
    CHECK(t->p);
    t->m = mirror_new();
}

static void term_fini(term_t *t) {
    ecpub_free(t->p);
    free(t->m);
}

static void term_cursor(term_t *t) {
    int col = t->ccol >= t->cols ? t->cols - 1 : t->ccol;
    CHECK(ecpub_set_cursor(t->p, (uint16_t)t->crow, (uint16_t)col) == ECPUB_OK);
}

static void term_print(term_t *t, const char *s) {
    char *line = t->lines[t->crow];
    size_t at = (size_t)t->ccol, n = strlen(s);
    if (at + n >= TCOLS) n = TCOLS - 1 - at;
    memcpy(line + at, s, n);
    line[at + n] = 0;
    t->ccol += (int)n;
    CHECK(ecpub_set_line(t->p, (uint16_t)t->crow, line, strlen(line)) == ECPUB_OK);
    term_cursor(t);
}

static void term_newline(term_t *t) {
    if (t->crow == t->rows - 1) {
        memmove(t->lines[0], t->lines[1], (size_t)(t->rows - 1) * TCOLS);
        t->lines[t->rows - 1][0] = 0;
        CHECK(ecpub_scroll(t->p, 1) == ECPUB_OK);
    } else {
        t->crow++;
    }
    t->ccol = 0;
    term_cursor(t);
}

static void term_osc(term_t *t, const char *payload, uint64_t now) {
    CHECK(ecpub_osc133(t->p, payload, strlen(payload), now) == ECPUB_OK);
}

/* Poll; apply and return 1 if a batch came out. */
static int term_poll(term_t *t, uint64_t now, ecpub_batch *out) {
    ecpub_batch b;
    int r = ecpub_poll(t->p, now, &b);
    CHECK(r == 0 || r == 1);
    if (r == 0) {
        CHECK(b.ops == NULL && b.len == 0);
        return 0;
    }
    apply(t->m, &b);
    if (out) *out = b;
    return 1;
}

/* Poll at the publisher's own deadline until it is idle. */
static uint64_t term_settle(term_t *t, uint64_t now) {
    for (int i = 0; i < 64; i++) {
        uint64_t d = ecpub_next_deadline(t->p);
        if (d == UINT64_MAX) return now;
        if (d > now) now = d;
        term_poll(t, now, NULL);
    }
    CHECK(!"publisher never settled");
    return now;
}

/* After a full publish, every published row must equal the emulator's. */
static void term_check_mirror(term_t *t) {
    for (int r = 0; r < t->rows; r++) {
        node_t *n = find(t->m, ECPUB_SLOT_BASE + (uint32_t)r);
        CHECK(n && n->role == ECPUB_ROLE_TERMINAL_LINE);
        /* the publisher trims trailing spaces */
        char want[TCOLS];
        snprintf(want, sizeof want, "%s", t->lines[r]);
        size_t l = strlen(want);
        while (l > 0 && want[l - 1] == ' ') want[--l] = 0;
        if (strcmp(n->text, want) != 0) {
            fprintf(stderr, "row %d: published '%s' want '%s'\n", r, n->text, want);
            CHECK(0);
        }
        CHECK(n->cursor == (r == t->crow ? t->ccol : -1));
    }
}

static void type_command(term_t *t, const char *a, const char *b, const char *c, const char *cmd, uint64_t now) {
    term_osc(t, a, now);
    term_print(t, "$ ");
    term_osc(t, b, now);
    term_print(t, cmd);
    term_newline(t);
    term_osc(t, c, now);
}

/* ------------------------------------------------------------------------ */
/* Fixtures                                                                 */
/* ------------------------------------------------------------------------ */

static void fixture_abi(void) {
    CHECK(ecpub_abi_version() == ECPUB_ABI_VERSION);
    CHECK(ecpub_op_size() == sizeof(ecpub_op));
    ecpub_config c;
    ecpub_config_default(&c);
    CHECK(c.min_interval_ns == 200 * MS); /* 5 Hz, P-04 §7 */
    CHECK(c.idle_ns == 100 * MS);         /* P-04 §7 */
    CHECK(c.downgrade_grace_ns == 500 * MS); /* S-05 §5.2 */
    puts("ok abi");
}

/* P-04 §8 throughput: 10 s of flat-out output. Bounded publish rate, bounded
 * memory, no generation storm, and the published grid still equals the real
 * one. `yes` repaints an identical screen, so line diffing has nothing to
 * send; `seq` changes every row, so the 5 Hz limit is what bounds it. */
static void fixture_throughput(const char *cmd, int distinct) {
    term_t t;
    term_init(&t, 24, 80);
    uint64_t now = 0;
    for (int i = 0; i < 30; i++) { /* earlier output, owned by the root */
        term_print(&t, "boot log line");
        term_newline(&t);
    }
    now = term_settle(&t, now);
    type_command(&t, "A", "B", "C", cmd, now);
    now = term_settle(&t, now + 1 * MS);
    CHECK(strcmp(ext(t.m, ECPUB_GROUP_BASE, "command"), cmd) == 0);

    uint64_t start = now, pubs0 = t.m->batches, struct0 = t.m->structural;
    ecpub_stats s1 = {0}, s10 = {0};
    uint64_t max_ops = 0, line = 0;
    char text[32];
    for (uint64_t ms = 1; ms <= 10000; ms++) {
        now = start + ms * MS;
        for (int k = 0; k < 50; k++) { /* 50 000 lines/s */
            if (distinct)
                snprintf(text, sizeof text, "%llu", (unsigned long long)++line);
            term_print(&t, distinct ? text : "y");
            term_newline(&t);
        }
        ecpub_batch b;
        if (term_poll(&t, now, &b) && b.len > max_ops) max_ops = b.len;
        if (ms == 1000) CHECK(ecpub_get_stats(t.p, &s1) == ECPUB_OK);
    }
    CHECK(ecpub_get_stats(t.p, &s10) == ECPUB_OK);
    uint64_t pubs = t.m->batches - pubs0, structural = t.m->structural - struct0;
    printf("   %s: %llu publishes, %llu structural, max %llu ops, %llu B retained\n",
           cmd, (unsigned long long)pubs, (unsigned long long)structural, (unsigned long long)max_ops,
           (unsigned long long)s10.retained_bytes);
    CHECK(pubs <= 51);                     /* <= 5 Hz over 10 s */
    if (distinct) CHECK(pubs >= 45);       /* and it did keep publishing */
    else CHECK(pubs <= 2);                 /* identical screen: nothing new */
    CHECK(structural <= 2);                /* the old rows leaving; no storm */
    CHECK(max_ops <= 3u * 24u + 8u);       /* line deltas, not the log */
    CHECK(s10.retained_bytes == s1.retained_bytes); /* flat after 1 s */
    CHECK(s10.retained_bytes < 512 * 1024);

    /* ^C: the block ends, the next prompt opens; the tree still matches. */
    term_osc(&t, "D;130", now);
    term_osc(&t, "A", now);
    term_print(&t, "$ ");
    now = term_settle(&t, now + 1 * MS);
    term_check_mirror(&t);
    CHECK(strcmp(ext(t.m, ECPUB_GROUP_BASE, "exit_code"), "130") == 0);
    term_fini(&t);
    printf("ok throughput %s\n", cmd);
}

/* P-04 §8 echo-off: the raise is published before any keystroke can be,
 * nothing leaves while raised, and it holds for downgrade_grace. */
static void echo_case(const char *name, const char *cmd, const char *prompt, int feedback) {
    term_t t;
    term_init(&t, 10, 80);
    uint64_t now = term_settle(&t, 0);
    type_command(&t, "A", "B", "C", cmd, now);
    now = term_settle(&t, now + 1 * MS);
    uint64_t values_before = t.m->value_ops, ext_before = t.m->ext_ops, batches_before = t.m->batches;

    /* getpass(): ECHO off, then the prompt. The very next poll is the raise,
     * even though a publish went out under a millisecond ago. */
    now += 1 * MS;
    CHECK(ecpub_set_echo(t.p, false, now) == ECPUB_OK);
    term_print(&t, prompt);
    CHECK(ecpub_next_deadline(t.p) == 0);
    ecpub_batch b;
    CHECK(term_poll(&t, now, &b) == 1);
    CHECK(b.flags & ECPUB_BATCH_URGENT);
    CHECK(b.len == 2);
    CHECK(b.ops[0].kind == ECPUB_OP_SET_EXT && b.ops[0].node == ECPUB_ROOT_ID);
    CHECK(strcmp(b.ops[0].key, "echo") == 0 && strcmp(b.ops[0].str, "false") == 0);
    CHECK(strcmp(ext(t.m, ECPUB_ROOT_ID, "echo"), "false") == 0);

    /* Keystrokes for 2 s: nothing at all is published, prompt included. */
    for (int i = 0; i < 8; i++) {
        now += 250 * MS;
        if (feedback) term_print(&t, "*");
        CHECK(ecpub_next_deadline(t.p) == UINT64_MAX);
        CHECK(term_poll(&t, now, NULL) == 0);
    }
    /* Enter: echo back on. Still raised for the grace. */
    now += 50 * MS;
    uint64_t on = now;
    CHECK(ecpub_set_echo(t.p, true, on) == ECPUB_OK);
    term_newline(&t);
    term_print(&t, "done");
    CHECK(ecpub_next_deadline(t.p) == on + 500 * MS);
    for (now = on; now < on + 500 * MS; now += 25 * MS) CHECK(term_poll(&t, now, NULL) == 0);
    CHECK(t.m->value_ops == values_before);
    CHECK(t.m->ext_ops == ext_before + 1);
    CHECK(t.m->batches == batches_before + 1);

    now = on + 500 * MS;
    CHECK(term_poll(&t, now, &b) == 1);
    CHECK(!(b.flags & ECPUB_BATCH_URGENT));
    CHECK(strcmp(ext(t.m, ECPUB_ROOT_ID, "echo"), "true") == 0);
    term_settle(&t, now);
    term_check_mirror(&t);
    term_fini(&t);
    printf("ok echo-off %s\n", name);
}

static void fixture_echo_flap(void) {
    term_t t;
    term_init(&t, 4, 40);
    /* ECHO off before the first publish: the raise carries the root only. */
    CHECK(ecpub_set_echo(t.p, false, 0) == ECPUB_OK);
    ecpub_batch b;
    CHECK(term_poll(&t, 0, &b) == 1);
    CHECK(b.len == 4 && b.ops[0].kind == ECPUB_OP_SET_ROOT && b.ops[2].kind == ECPUB_OP_SET_EXT);
    CHECK(t.m->value_ops == 0);
    /* on, then off again inside the grace: one raise, grace restarts. */
    CHECK(ecpub_set_echo(t.p, true, 100 * MS) == ECPUB_OK);
    CHECK(term_poll(&t, 400 * MS, NULL) == 0);
    CHECK(ecpub_set_echo(t.p, false, 450 * MS) == ECPUB_OK);
    CHECK(term_poll(&t, 450 * MS, NULL) == 0);
    CHECK(term_poll(&t, 700 * MS, NULL) == 0);
    CHECK(ecpub_set_echo(t.p, true, 800 * MS) == ECPUB_OK);
    CHECK(term_poll(&t, 1299 * MS, NULL) == 0);
    CHECK(term_poll(&t, 1300 * MS, NULL) == 1);
    CHECK(t.m->urgent == 1);
    CHECK(strcmp(ext(t.m, ECPUB_ROOT_ID, "echo"), "true") == 0);
    /* A config cannot shorten the grace. */
    ecpub_config c;
    ecpub_config_default(&c);
    c.downgrade_grace_ns = 0;
    ecpub_publisher *p = ecpub_new(&c, 4, 40);
    CHECK(ecpub_poll(p, 0, &b) == 1);
    ecpub_set_echo(p, false, 0);
    CHECK(ecpub_poll(p, 0, &b) == 1 && (b.flags & ECPUB_BATCH_URGENT));
    ecpub_set_echo(p, true, 1 * MS);
    CHECK(ecpub_poll(p, 400 * MS, &b) == 0);
    CHECK(ecpub_poll(p, 501 * MS, &b) == 1);
    ecpub_free(p);
    term_fini(&t);
    puts("ok echo-off flap");
}

typedef struct {
    const char *name, *a, *b, *c, *d_fmt;
} shell_t;

static uint32_t group_at(mirror_t *m, int k) {
    node_t *root = find(m, ECPUB_ROOT_ID);
    for (int i = 0; i < root->nchild; i++) {
        node_t *n = find(m, root->children[i]);
        if (n->role == ECPUB_ROLE_GROUP && k-- == 0) return n->id;
    }
    return 0;
}

static int ngroups(mirror_t *m) {
    int k = 0;
    while (group_at(m, k)) k++;
    return k;
}

static const char *child_text(mirror_t *m, uint32_t g, int i) {
    node_t *n = find(m, g);
    if (!n || i >= n->nchild) return NULL;
    return find(m, n->children[i])->text;
}

/* P-04 §8 OSC 133: command blocks, exit codes and prompt detection for the
 * marker dialects bash, zsh and fish emit. */
static void osc_case(const shell_t *sh) {
    term_t t;
    term_init(&t, 12, 80);
    uint64_t now = term_settle(&t, 0);
    struct {
        const char *cmd, *out;
        int exit;
    } runs[] = {{"echo hello", "hello", 0}, {"false", NULL, 1}, {"ls /nope", "ls: cannot access '/nope'", 2}};
    char d[64];
    for (int i = 0; i < 3; i++) {
        type_command(&t, sh->a, sh->b, sh->c, runs[i].cmd, now);
        now = term_settle(&t, now + 1 * MS);
        if (runs[i].out) {
            term_print(&t, runs[i].out);
            term_newline(&t);
        }
        snprintf(d, sizeof d, sh->d_fmt, runs[i].exit);
        term_osc(&t, d, now);
        now = term_settle(&t, now + 1 * MS);
    }
    term_osc(&t, sh->a, now);
    term_print(&t, "$ ");
    term_osc(&t, sh->b, now);
    term_print(&t, "gi");
    now = term_settle(&t, now + 1 * MS);
    term_check_mirror(&t);

    CHECK(ngroups(t.m) == 4);
    for (int i = 0; i < 3; i++) {
        uint32_t g = group_at(t.m, i);
        CHECK(find(t.m, g)->role == ECPUB_ROLE_GROUP);
        CHECK(strcmp(ext(t.m, g, "command"), runs[i].cmd) == 0);
        snprintf(d, sizeof d, "%d", runs[i].exit);
        CHECK(strcmp(ext(t.m, g, "exit_code"), d) == 0);
        CHECK(strcmp(ext(t.m, g, "is_prompt"), "false") == 0);
        CHECK(ext(t.m, g, "started") && ext(t.m, g, "ended"));
        char want[128];
        snprintf(want, sizeof want, "$ %s", runs[i].cmd);
        CHECK(strcmp(child_text(t.m, g, 0), want) == 0);
        CHECK(find(t.m, g)->nchild == (runs[i].out ? 2 : 1));
        if (runs[i].out) CHECK(strcmp(child_text(t.m, g, 1), runs[i].out) == 0);
        CHECK(strcmp(ext(t.m, find(t.m, g)->children[0], "is_prompt"), "false") == 0);
    }
    uint32_t cur = group_at(t.m, 3);
    CHECK(strcmp(ext(t.m, cur, "is_prompt"), "true") == 0);
    CHECK(ext(t.m, cur, "command") == NULL && ext(t.m, cur, "exit_code") == NULL);
    CHECK(find(t.m, cur)->nchild == 1);
    CHECK(strcmp(child_text(t.m, cur, 0), "$ gi") == 0);
    CHECK(strcmp(ext(t.m, find(t.m, cur)->children[0], "is_prompt"), "true") == 0);
    /* Rows below the cursor are nobody's output. */
    CHECK(find(t.m, ECPUB_SLOT_BASE + 11)->parent == ECPUB_ROOT_ID);

    /* Alternate screen: markers ignored, rows leave the groups, and come
     * back on exit. */
    uint64_t s0 = t.m->structural;
    CHECK(ecpub_set_alt_screen(t.p, true) == ECPUB_OK);
    term_osc(&t, sh->a, now);
    now = term_settle(&t, now + 1 * MS);
    CHECK(t.m->structural == s0 + 1);
    CHECK(strcmp(ext(t.m, ECPUB_ROOT_ID, "alt_screen"), "true") == 0);
    for (int i = 0; i < 4; i++) CHECK(find(t.m, group_at(t.m, i))->nchild == 0);
    CHECK(ecpub_set_alt_screen(t.p, false) == ECPUB_OK);
    now = term_settle(&t, now + 1 * MS);
    CHECK(ngroups(t.m) == 4 && find(t.m, cur)->nchild == 1);
    term_check_mirror(&t);
    term_fini(&t);
    printf("ok osc133 %s\n", sh->name);
}

static void fixture_long_command(void) {
    term_t t;
    term_init(&t, 8, 400);
    uint64_t now = term_settle(&t, 0);
    char cmd[390];
    memset(cmd, 'x', sizeof cmd - 1);
    cmd[sizeof cmd - 1] = 0;
    type_command(&t, "A", "B", "C", cmd, now);
    term_settle(&t, now + 1 * MS);
    const char *c = ext(t.m, ECPUB_GROUP_BASE, "command");
    CHECK(c && strlen(c) == ECPUB_EXT_VALUE_MAX);
    CHECK(strcmp(ext(t.m, ECPUB_GROUP_BASE, "command_truncated"), "true") == 0);
    term_fini(&t);
    puts("ok osc133 long command");
}

/* A program spamming OSC 133 cannot drive a publish or generation storm. */
static void fixture_marker_flood(void) {
    term_t t;
    term_init(&t, 24, 80);
    uint64_t now = 0;
    for (uint64_t ms = 1; ms <= 10000; ms++) {
        now = ms * MS;
        for (int k = 0; k < 20; k++) {
            term_osc(&t, "A", now);
            term_print(&t, "x");
            term_osc(&t, "C", now);
            term_newline(&t);
            term_osc(&t, "D;0", now);
        }
        term_poll(&t, now, NULL);
    }
    ecpub_stats s;
    CHECK(ecpub_get_stats(t.p, &s) == ECPUB_OK);
    printf("   flood: %llu publishes, %llu blocks live, %llu B retained\n", (unsigned long long)t.m->batches,
           (unsigned long long)s.blocks, (unsigned long long)s.retained_bytes);
    CHECK(t.m->batches <= 101); /* 10 Hz boundary cap, P-07 §3 */
    CHECK(s.blocks <= 64);
    CHECK(s.retained_bytes < 1024 * 1024);
    term_settle(&t, now);
    term_check_mirror(&t);
    term_fini(&t);
    puts("ok osc133 flood");
}

/* ec_cataclysm_pub.h ownership and lifetime rules. */
static void fixture_lifetime(void) {
    ecpub_batch b;
    ecpub_stats s;
    /* NULL is refused, never dereferenced. */
    ecpub_free(NULL);
    ecpub_config_default(NULL);
    CHECK(ecpub_resize(NULL, 1, 1) == ECPUB_ERR_NULL);
    CHECK(ecpub_set_line(NULL, 0, "x", 1) == ECPUB_ERR_NULL);
    CHECK(ecpub_scroll(NULL, 1) == ECPUB_ERR_NULL);
    CHECK(ecpub_set_cursor(NULL, 0, 0) == ECPUB_ERR_NULL);
    CHECK(ecpub_set_alt_screen(NULL, true) == ECPUB_ERR_NULL);
    CHECK(ecpub_set_echo(NULL, false, 0) == ECPUB_ERR_NULL);
    CHECK(ecpub_osc133(NULL, "A", 1, 0) == ECPUB_ERR_NULL);
    CHECK(ecpub_poll(NULL, 0, &b) == ECPUB_ERR_NULL);
    CHECK(ecpub_next_deadline(NULL) == UINT64_MAX);
    CHECK(ecpub_get_stats(NULL, &s) == ECPUB_ERR_NULL);
    /* Sizes. */
    CHECK(ecpub_new(NULL, 0, 80) == NULL);
    CHECK(ecpub_new(NULL, 24, 0) == NULL);
    CHECK(ecpub_new(NULL, ECPUB_MAX_ROWS + 1, 80) == NULL);
    CHECK(ecpub_new(NULL, 24, ECPUB_MAX_COLS + 1) == NULL);

    ecpub_publisher *p = ecpub_new(NULL, 3, 10);
    CHECK(p);
    CHECK(ecpub_poll(p, 0, NULL) == ECPUB_ERR_NULL);
    CHECK(ecpub_get_stats(p, NULL) == ECPUB_ERR_NULL);
    CHECK(ecpub_set_line(p, 3, "x", 1) == ECPUB_ERR_RANGE);
    CHECK(ecpub_set_cursor(p, 0, 10) == ECPUB_ERR_RANGE);
    CHECK(ecpub_resize(p, 0, 10) == ECPUB_ERR_RANGE);
    CHECK(ecpub_set_line(p, 0, NULL, 5) == ECPUB_ERR_NULL);
    CHECK(ecpub_set_line(p, 0, NULL, 0) == ECPUB_OK);
    CHECK(ecpub_osc133(p, NULL, 3, 0) == ECPUB_ERR_NULL);

    /* Input is copied: the caller may reuse its buffer at once. Hostile
     * bytes (NUL, ESC, C1, bad UTF-8, oversize) come out as safe text. */
    char buf[8000];
    strcpy(buf, "abc");
    CHECK(ecpub_set_line(p, 0, buf, 3) == ECPUB_OK);
    strcpy(buf, "zzz");
    const char hostile[] = "a\0b\x1b]133;D\x07\xc2\x9b\xff\xfe end";
    CHECK(ecpub_set_line(p, 1, hostile, sizeof hostile - 1) == ECPUB_OK);
    memset(buf, 'w', sizeof buf);
    CHECK(ecpub_set_line(p, 2, buf, sizeof buf) == ECPUB_OK);
    mirror_t *m = mirror_new();
    CHECK(ecpub_poll(p, 0, &b) == 1);
    apply(m, &b); /* validates every string in the batch */
    CHECK(strcmp(find(m, ECPUB_SLOT_BASE)->text, "abc") == 0);
    CHECK(strlen(find(m, ECPUB_SLOT_BASE + 2)->text) == ECPUB_MAX_LINE_BYTES);
    CHECK(strstr(find(m, ECPUB_SLOT_BASE + 1)->text, "end") != NULL);
    /* The batch stays readable until the next mutating call: read it twice. */
    size_t total = 0;
    for (int pass = 0; pass < 2; pass++)
        for (size_t i = 0; i < b.len; i++) total += b.ops[i].str_len + b.ops[i].key_len;
    CHECK(total > 0);
    CHECK(ecpub_get_stats(p, &s) == ECPUB_OK); /* const calls keep it valid */
    (void)ecpub_next_deadline(p);
    for (size_t i = 0; i < b.len; i++)
        if (b.ops[i].str) CHECK(strlen(b.ops[i].str) == b.ops[i].str_len);
    ecpub_free(p);
    free(m);

    /* Many publishers, each driven and freed: LeakSanitizer checks the rest. */
    for (int k = 0; k < 50; k++) {
        term_t t;
        term_init(&t, 5 + k % 7, 20 + k);
        uint64_t now = term_settle(&t, 0);
        type_command(&t, "A", "B", "C", "make", now);
        term_print(&t, "building");
        term_newline(&t);
        CHECK(ecpub_resize(t.p, 4, 30) == ECPUB_OK);
        t.rows = 4;
        if (t.crow > 3) t.crow = 3;
        t.ccol = 0;
        term_cursor(&t);
        for (int r = 0; r < 4; r++)
            CHECK(ecpub_set_line(t.p, (uint16_t)r, t.lines[r], strlen(t.lines[r])) == ECPUB_OK);
        term_settle(&t, now + 1 * MS);
        term_check_mirror(&t);
        term_fini(&t);
    }
    puts("ok lifetime");
}

int main(void) {
    fixture_abi();
    fixture_lifetime();
    fixture_throughput("yes", 0);
    fixture_throughput("seq 1 inf", 1);
    echo_case("sudo", "sudo true", "[sudo] password for user: ", 1);
    echo_case("ssh", "ssh host", "user@host's password: ", 0);
    echo_case("read -s", "read -s pw", "Password: ", 0);
    fixture_echo_flap();
    static const shell_t shells[] = {
        {"bash", "A", "B", "C", "D;%d"},
        {"zsh", "A;cl=m;aid=4242", "B", "C", "133;D;%d;aid=4242"},
        {"fish", "A;special_key=1", "B", "C;cmdline_url=x", "D;%d"},
    };
    for (size_t i = 0; i < sizeof shells / sizeof shells[0]; i++) osc_case(&shells[i]);
    fixture_long_command();
    fixture_marker_flood();
    puts("all fixtures passed");
    return 0;
}
