/*
 * Guest runner. Forks one static test harness, records the executable
 * mapping, and writes stdout, stderr, and the exit code into the scratch
 * directory. It does not emit fences.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <unistd.h>

static void trim(char *s)
{
    size_t n = strlen(s);
    while (n > 0 && (s[n - 1] == '\n' || s[n - 1] == '\r')) {
        s[--n] = 0;
    }
}

static int write_maps(pid_t pid, const char *exe, const char *map_path)
{
    char proc[64];
    FILE *in;
    FILE *out;
    char line[1024];
    int found = 0;

    snprintf(proc, sizeof proc, "/proc/%d/maps", (int)pid);
    in = fopen(proc, "r");
    if (!in) {
        return -1;
    }
    out = fopen(map_path, "w");
    if (!out) {
        fclose(in);
        return -1;
    }
    while (fgets(line, sizeof line, in)) {
        unsigned long start = 0, end = 0, offset = 0;
        char perms[8];
        if (sscanf(line, "%lx-%lx %7s %lx", &start, &end, perms, &offset) < 4) {
            continue;
        }
        if (perms[0] == 'r' && perms[2] == 'x' && strstr(line, exe)) {
            fprintf(out, "bias 0x%lx\nstart 0x%lx\nend 0x%lx\n", start - offset, start, end);
            found = 1;
            break;
        }
    }
    fclose(in);
    fclose(out);
    return found ? 0 : -1;
}

static void write_exit(const char *path, int status)
{
    FILE *f = fopen(path, "w");
    int code;

    if (!f) {
        return;
    }
    if (WIFEXITED(status)) {
        code = WEXITSTATUS(status);
    } else if (WIFSIGNALED(status)) {
        code = 128 + WTERMSIG(status);
    } else {
        code = 128;
    }
    fprintf(f, "%d\n", code);
    fclose(f);
}

int main(int argc, char **argv)
{
    FILE *job;
    char argbuf[64][1024];
    char *args[65];
    int nargs = 0;
    char stdout_path[512], stderr_path[512], exit_path[512], map_path[512];
    pid_t pid;
    int status = 0;

    if (argc != 3) {
        fprintf(stderr, "usage: runner <job> <scratch>\n");
        return 2;
    }
    job = fopen(argv[1], "r");
    if (!job) {
        perror(argv[1]);
        return 3;
    }
    if (!fgets(argbuf[0], sizeof argbuf[0], job)) {
        fprintf(stderr, "empty job\n");
        return 4;
    }
    trim(argbuf[0]);
    nargs = 1;
    while (nargs < 64 && fgets(argbuf[nargs], sizeof argbuf[0], job)) {
        trim(argbuf[nargs]);
        if (argbuf[nargs][0] == 0) {
            continue;
        }
        nargs++;
    }
    fclose(job);
    for (int i = 0; i < nargs; i++) {
        args[i] = argbuf[i];
    }
    args[nargs] = NULL;

    snprintf(stdout_path, sizeof stdout_path, "%s/stdout", argv[2]);
    snprintf(stderr_path, sizeof stderr_path, "%s/stderr", argv[2]);
    snprintf(exit_path, sizeof exit_path, "%s/exit", argv[2]);
    snprintf(map_path, sizeof map_path, "%s/map", argv[2]);

    pid = fork();
    if (pid < 0) {
        perror("fork");
        return 5;
    }
    if (pid == 0) {
        int outfd = open(stdout_path, O_CREAT | O_TRUNC | O_WRONLY, 0644);
        int errfd = open(stderr_path, O_CREAT | O_TRUNC | O_WRONLY, 0644);
        if (outfd >= 0) {
            dup2(outfd, 1);
        }
        if (errfd >= 0) {
            dup2(errfd, 2);
        }
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) < 0) {
            /* Still exec. The parent falls back if it cannot stop us. */
        }
        execv(args[0], args);
        perror("exec");
        _exit(127);
    }

    if (waitpid(pid, &status, WUNTRACED) < 0) {
        perror("waitpid");
        return 6;
    }
    if (WIFSTOPPED(status)) {
        write_maps(pid, args[0], map_path);
        ptrace(PTRACE_DETACH, pid, NULL, NULL);
        if (waitpid(pid, &status, WUNTRACED) < 0) {
            perror("waitpid");
            return 6;
        }
    } else {
        write_maps(pid, args[0], map_path);
    }
    write_exit(exit_path, status);
    return 0;
}
