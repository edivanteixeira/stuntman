// A disk where each flush costs STUNTMAN_FSYNC_MS (default 6 ms), like a cloud managed disk.
// Build: gcc -shared -fPIC -O2 -o slow_fsync.so slow_fsync.c -ldl ; run with LD_PRELOAD=slow_fsync.so
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <time.h>

static void wait_disk(void) {
    const char *v = getenv("STUNTMAN_FSYNC_MS");
    long ms = v ? atol(v) : 6;
    struct timespec t = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&t, NULL);
}

int fsync(int fd) {
    static int (*real)(int);
    if (!real) real = dlsym(RTLD_NEXT, "fsync");
    wait_disk();
    return real(fd);
}

int fdatasync(int fd) {
    static int (*real)(int);
    if (!real) real = dlsym(RTLD_NEXT, "fdatasync");
    wait_disk();
    return real(fd);
}
