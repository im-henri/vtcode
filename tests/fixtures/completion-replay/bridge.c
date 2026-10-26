/* Test-only Linux LD_PRELOAD shim. Never shipped or loaded by VTCode.
 * Delay I/O only on descriptors/paths under an explicitly configured directory.
 * Use syscalls for descriptor lookup and sleeping to avoid interposition loops.
 */
#define _GNU_SOURCE
#define _LARGEFILE64_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static char root[PATH_MAX];
static size_t root_len;
static unsigned long delay_us;

__attribute__((constructor)) static void configure(void) {
    const char *path = getenv("VTCODE_LAB_DISK_ROOT");
    const char *delay = getenv("VTCODE_LAB_DISK_DELAY_US");
    if (!path || !delay || !realpath(path, root)) return;
    root_len = strlen(root);
    /* Refuse a global filesystem delay, including an accidental root path. */
    if (root_len < 2) { root_len = 0; return; }
    delay_us = strtoul(delay, NULL, 10);
}

static int inside(const char *path) {
    return root_len && !strncmp(path, root, root_len) &&
           (path[root_len] == '/' || path[root_len] == '\0');
}

static void pause_io(void) {
    int saved = errno;
    struct timespec ts = { delay_us / 1000000, (delay_us % 1000000) * 1000 };
    while (syscall(SYS_nanosleep, &ts, &ts) < 0 && errno == EINTR) {}
    errno = saved;
}

static void delay_fd(int fd) {
    if (!delay_us || !root_len) return;
    int saved = errno;
    char link[64], path[PATH_MAX];
    snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
    ssize_t n = syscall(SYS_readlinkat, AT_FDCWD, link, path, sizeof(path) - 1);
    if (n > 0) { path[n] = 0; if (inside(path)) pause_io(); }
    errno = saved;
}

static void delay_path(int dirfd, const char *path) {
    if (!delay_us || !root_len || !path) return;
    int saved = errno;
    char base[PATH_MAX], joined[PATH_MAX], resolved[PATH_MAX];
    if (path[0] == '/') snprintf(joined, sizeof(joined), "%s", path);
    else {
        if (dirfd == AT_FDCWD) {
            if (syscall(SYS_getcwd, base, sizeof(base)) < 0) goto done;
        } else {
            char link[64];
            snprintf(link, sizeof(link), "/proc/self/fd/%d", dirfd);
            ssize_t n = syscall(SYS_readlinkat, AT_FDCWD, link, base, sizeof(base) - 1);
            if (n <= 0) goto done;
            base[n] = 0;
        }
        if (snprintf(joined, sizeof(joined), "%s/%s", base, path) >= (int)sizeof(joined)) goto done;
    }
    if (realpath(joined, resolved)) {
        if (inside(resolved)) pause_io();
    } else {
        /* Newly created file: canonicalize its existing parent first. */
        char *slash = strrchr(joined, '/');
        if (slash) { *slash = 0; if (realpath(joined, resolved) && inside(resolved)) pause_io(); }
    }
done:
    errno = saved;
}

/* Constructor-resolved immutable pointers avoid data races on concurrent I/O. */
#define DECL(ret, name, args) static ret (*real_##name) args
DECL(ssize_t, read, (int, void *, size_t));
DECL(ssize_t, write, (int, const void *, size_t));
DECL(ssize_t, pread64, (int, void *, size_t, off64_t));
DECL(ssize_t, pwrite64, (int, const void *, size_t, off64_t));
DECL(int, fsync, (int));
DECL(int, fdatasync, (int));
DECL(int, open, (const char *, int, ...));
DECL(int, open64, (const char *, int, ...));
DECL(int, openat, (int, const char *, int, ...));
DECL(int, openat64, (int, const char *, int, ...));
DECL(int, statx, (int, const char *, int, unsigned int, struct statx *));
DECL(int, rename, (const char *, const char *));
DECL(int, unlink, (const char *));

__attribute__((constructor)) static void resolve(void) {
#define RESOLVE(name) real_##name = dlsym(RTLD_NEXT, #name)
    RESOLVE(read); RESOLVE(write); RESOLVE(pread64); RESOLVE(pwrite64);
    RESOLVE(fsync); RESOLVE(fdatasync); RESOLVE(open); RESOLVE(open64);
    RESOLVE(openat); RESOLVE(openat64); RESOLVE(statx); RESOLVE(rename); RESOLVE(unlink);
}

ssize_t read(int fd, void *buf, size_t n) { delay_fd(fd); return real_read(fd, buf, n); }
ssize_t write(int fd, const void *buf, size_t n) { delay_fd(fd); return real_write(fd, buf, n); }
ssize_t pread64(int fd, void *buf, size_t n, off64_t o) { delay_fd(fd); return real_pread64(fd, buf, n, o); }
ssize_t pwrite64(int fd, const void *buf, size_t n, off64_t o) { delay_fd(fd); return real_pwrite64(fd, buf, n, o); }
int fsync(int fd) { delay_fd(fd); return real_fsync(fd); }
int fdatasync(int fd) { delay_fd(fd); return real_fdatasync(fd); }

#define OPEN(name) int name(const char *path, int flags, ...) { \
    mode_t mode = 0; \
    if ((flags & O_CREAT) || (flags & O_TMPFILE) == O_TMPFILE) { \
        va_list a; va_start(a, flags); mode = va_arg(a, mode_t); va_end(a); } \
    delay_path(AT_FDCWD, path); return real_##name(path, flags, mode); }
OPEN(open)
OPEN(open64)
#define OPENAT(name) int name(int fd, const char *path, int flags, ...) { \
    mode_t mode = 0; \
    if ((flags & O_CREAT) || (flags & O_TMPFILE) == O_TMPFILE) { \
        va_list a; va_start(a, flags); mode = va_arg(a, mode_t); va_end(a); } \
    delay_path(fd, path); return real_##name(fd, path, flags, mode); }
OPENAT(openat)
OPENAT(openat64)
int statx(int fd, const char *path, int flags, unsigned int mask, struct statx *buf) {
    if (path && *path) delay_path(fd, path); else delay_fd(fd);
    return real_statx(fd, path, flags, mask, buf);
}
int rename(const char *from, const char *to) { delay_path(AT_FDCWD, from); delay_path(AT_FDCWD, to); return real_rename(from, to); }
int unlink(const char *path) { delay_path(AT_FDCWD, path); return real_unlink(path); }
