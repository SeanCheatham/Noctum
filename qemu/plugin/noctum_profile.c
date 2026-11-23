/*
 * TCG plugin for one Noctum measurement.
 *
 * Counts guest instruction callbacks between two ARM64 markers.
 * The markers are excluded. A second marker pair is an error.
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
static int flushed;

static uint64_t *counter_for(uint64_t pc)
{
    uint64_t *existing = g_hash_table_lookup(hist, &pc);
    if (existing) return existing;
    uint64_t *key = g_new(uint64_t, 1);
    uint64_t *value = g_new0(uint64_t, 1);
    *key = pc;
    g_hash_table_insert(hist, key, value);
    return value;
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
        if (n) fprintf(f, "bb 0x%" PRIx64 " %" PRIu64 "\n", pc, n);
    }
    if (state == ST_ERROR) {
        fprintf(f, "error second_fence\n");
    } else if (state == ST_ARMED) {
        fprintf(f, "error unclosed\n");
    } else if (state == ST_IDLE) {
        fprintf(f, "error no_fence\n");
    }
    fclose(f);
    flushed = 1;
}

static int insn_is_fence(struct qemu_plugin_insn *insn)
{
    uint8_t buf[16];
    size_t n = qemu_plugin_insn_data(insn, buf, sizeof buf);

    if (n == 4 && buf[0] == 0x3f && buf[1] == 0xc6 && buf[2] == 0x89 && buf[3] == 0xd2) return 1;
    if (n == 4 && buf[0] == 0x5f && buf[1] == 0xc6 && buf[2] == 0x89 && buf[3] == 0xd2) return 2;
    return 0;
}

static void on_fence(unsigned int vcpu_index, void *userdata)
{
    int al = (int)(uintptr_t)userdata;

    (void)vcpu_index;
    (void)userdata;
    if (al == 1) {
        if (state != ST_IDLE) {
            state = ST_ERROR;
            flush_dump();
            return;
        }
        GHashTableIter iter;
        gpointer key, value;
        g_hash_table_iter_init(&iter, hist);
        while (g_hash_table_iter_next(&iter, &key, &value)) {
            *(uint64_t *)value = 0;
        }
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
    (void)vcpu_index;
    if (state == ST_ARMED) {
        ++*(uint64_t *)userdata;
        ++total;
    }
}

static void tb_trans(qemu_plugin_id_t id, struct qemu_plugin_tb *tb)
{
    size_t n = qemu_plugin_tb_n_insns(tb);
    size_t i;

    (void)id;
    for (i = 0; i < n; i++) {
        struct qemu_plugin_insn *insn = qemu_plugin_tb_get_insn(tb, i);
        if (insn_is_fence(insn)) {
            qemu_plugin_register_vcpu_insn_exec_cb(insn, on_fence, QEMU_PLUGIN_CB_NO_REGS,
                                                   (void *)(uintptr_t)insn_is_fence(insn));
        } else {
            uint64_t *counter = counter_for(qemu_plugin_insn_vaddr(insn));
            qemu_plugin_register_vcpu_insn_exec_cb(insn, on_insn, QEMU_PLUGIN_CB_NO_REGS,
                                                   counter);
        }
    }
}

static void on_plugin_exit(qemu_plugin_id_t id, void *userdata)
{
    (void)id;
    (void)userdata;
    flush_dump();
}

QEMU_PLUGIN_EXPORT int qemu_plugin_install(qemu_plugin_id_t id, const qemu_info_t *info,
                                           int argc, char **argv)
{
    int i;

    if (strcmp(info->target_name, "aarch64") != 0) return -1;
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
    qemu_plugin_register_vcpu_tb_trans_cb(id, tb_trans);
    qemu_plugin_register_atexit_cb(id, on_plugin_exit, NULL);
    return 0;
}
