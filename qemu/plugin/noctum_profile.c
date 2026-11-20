/*
 * TCG plugin for one Noctum measurement.
 *
 * Counts retired guest instructions while armed. An `out 0xe9, al` (bytes
 * E6 E9) with AL=1 zeroes the counters and arms. AL=2 disarms and flushes.
 * The fence instructions themselves are not counted. A second fence pair in
 * the same process is a hard error.
 *
 * SPDX-License-Identifier: GPL-2.0-or-later
 *
 * This file is a QEMU plugin. It is GPL-2.0-or-later because it is built
 * against QEMU's plugin API. It is not linked into the Noctum binary.
 */

#include <glib.h>
#include <inttypes.h>
#include <stdio.h>
#include <string.h>

#include <qemu-plugin.h>

QEMU_PLUGIN_EXPORT int qemu_plugin_version = QEMU_PLUGIN_VERSION;

enum {
    ST_IDLE = 0,
    ST_ARMED = 1,
    ST_DONE = 2,
    ST_ERROR = 3
};

static int state = ST_IDLE;
static uint64_t total;
static GHashTable *hist;
static char *output_path;
static struct qemu_plugin_register *rax_handle;
static int flushed;

static void add_count(uint64_t pc, uint64_t n)
{
    uint64_t *key = g_new(uint64_t, 1);
    uint64_t *existing;

    *key = pc;
    existing = g_hash_table_lookup(hist, key);
    if (existing) {
        *existing += n;
        g_free(key);
    } else {
        uint64_t *val = g_new(uint64_t, 1);
        *val = n;
        g_hash_table_insert(hist, key, val);
    }
    total += n;
}

static void flush_dump(void)
{
    FILE *f;
    GHashTableIter iter;
    gpointer k, v;

    if (flushed || !output_path) {
        return;
    }
    f = fopen(output_path, "w");
    if (!f) {
        qemu_plugin_outs("noctum_profile: failed to open output\n");
        return;
    }
    fprintf(f, "total %" PRIu64 "\n", total);
    g_hash_table_iter_init(&iter, hist);
    while (g_hash_table_iter_next(&iter, &k, &v)) {
        uint64_t pc = *(uint64_t *)k;
        uint64_t n = *(uint64_t *)v;
        fprintf(f, "bb 0x%" PRIx64 " %" PRIu64 "\n", pc, n);
    }
    if (state == ST_ERROR) {
        fprintf(f, "error second_fence\n");
    } else if (state == ST_ARMED) {
        fprintf(f, "error unclosed\n");
    }
    fclose(f);
    flushed = 1;
}

static int insn_is_fence(struct qemu_plugin_insn *insn)
{
    uint8_t buf[16];
    size_t n = qemu_plugin_insn_data(insn, buf, sizeof buf);
    size_t i;

    for (i = 0; i + 1 < n; i++) {
        if (buf[i] == 0xE6 && buf[i + 1] == 0xE9) {
            return 1;
        }
    }
    return 0;
}

static int read_al(void)
{
    GByteArray *buf;
    int n;
    int al;

    if (!rax_handle) {
        return -1;
    }
    buf = g_byte_array_new();
    n = qemu_plugin_read_register(rax_handle, buf);
    if (n < 1 || buf->len < 1) {
        g_byte_array_free(buf, TRUE);
        return -1;
    }
    /* Target byte order is little-endian on x86. AL is the low byte of RAX. */
    al = buf->data[0];
    g_byte_array_free(buf, TRUE);
    return al;
}

static void on_fence(unsigned int vcpu_index, void *userdata)
{
    int al = read_al();

    (void)vcpu_index;
    (void)userdata;
    if (al == 1) {
        if (state != ST_IDLE) {
            state = ST_ERROR;
            flush_dump();
            return;
        }
        g_hash_table_remove_all(hist);
        total = 0;
        state = ST_ARMED;
        return;
    }
    if (al == 2) {
        if (state != ST_ARMED) {
            state = ST_ERROR;
            flush_dump();
            return;
        }
        state = ST_DONE;
        flush_dump();
    }
}

static void on_insn(unsigned int vcpu_index, void *userdata)
{
    uint64_t pc = *(uint64_t *)userdata;

    (void)vcpu_index;
    if (state == ST_ARMED) {
        add_count(pc, 1);
    }
}

static void on_block(unsigned int vcpu_index, void *userdata)
{
    uint64_t *info = userdata;

    (void)vcpu_index;
    if (state == ST_ARMED) {
        add_count(info[0], info[1]);
    }
}

static void vcpu_init(qemu_plugin_id_t id, unsigned int vcpu_index)
{
    GArray *regs;
    guint i;

    (void)id;
    (void)vcpu_index;
    regs = qemu_plugin_get_registers();
    for (i = 0; i < regs->len; i++) {
        qemu_plugin_reg_descriptor *reg =
            &g_array_index(regs, qemu_plugin_reg_descriptor, i);
        if (reg->name && strcmp(reg->name, "rax") == 0) {
            rax_handle = reg->handle;
            break;
        }
    }
    g_array_free(regs, TRUE);
}

static void tb_trans(qemu_plugin_id_t id, struct qemu_plugin_tb *tb)
{
    size_t n = qemu_plugin_tb_n_insns(tb);
    size_t i;
    int has_fence = 0;

    (void)id;
    for (i = 0; i < n; i++) {
        if (insn_is_fence(qemu_plugin_tb_get_insn(tb, i))) {
            has_fence = 1;
            break;
        }
    }

    if (!has_fence) {
        struct qemu_plugin_insn *first = qemu_plugin_tb_get_insn(tb, 0);
        uint64_t *info = g_new(uint64_t, 2);
        info[0] = qemu_plugin_insn_vaddr(first);
        info[1] = (uint64_t)n;
        qemu_plugin_register_vcpu_tb_exec_cb(tb, on_block, QEMU_PLUGIN_CB_NO_REGS,
                                             info);
        return;
    }

    for (i = 0; i < n; i++) {
        struct qemu_plugin_insn *insn = qemu_plugin_tb_get_insn(tb, i);
        if (insn_is_fence(insn)) {
            qemu_plugin_register_vcpu_insn_exec_cb(insn, on_fence, QEMU_PLUGIN_CB_R_REGS,
                                                   NULL);
        } else {
            uint64_t *pc = g_new(uint64_t, 1);
            *pc = qemu_plugin_insn_vaddr(insn);
            qemu_plugin_register_vcpu_insn_exec_cb(insn, on_insn, QEMU_PLUGIN_CB_NO_REGS,
                                                   pc);
        }
    }
}

static void on_exit(qemu_plugin_id_t id, void *userdata)
{
    (void)id;
    (void)userdata;
    flush_dump();
}

QEMU_PLUGIN_EXPORT int qemu_plugin_install(qemu_plugin_id_t id, const qemu_info_t *info,
                                           int argc, char **argv)
{
    int i;

    (void)info;
    hist = g_hash_table_new_full(g_int64_hash, g_int64_equal, g_free, g_free);
    for (i = 0; i < argc; i++) {
        if (g_str_has_prefix(argv[i], "output=")) {
            output_path = g_strdup(argv[i] + strlen("output="));
        }
    }
    if (!output_path) {
        qemu_plugin_outs("noctum_profile: missing output= argument\n");
        return -1;
    }
    qemu_plugin_register_vcpu_init_cb(id, vcpu_init);
    qemu_plugin_register_vcpu_tb_trans_cb(id, tb_trans);
    qemu_plugin_register_atexit_cb(id, on_exit, NULL);
    return 0;
}
