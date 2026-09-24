/* Linux kernel-parity guards for the local runtime. Originated as
 * renameat2(RENAME_NOREPLACE) — atomic no-overwrite rename for immutable
 * generation install (Node fs.renameSync would silently replace an
 * existing empty target directory; this primitive fails EEXIST instead).
 * Now also carries flockfd / statfsmagic / openat_* / unlinkat /
 * getpeereuid — the primitives src/local_runtime and
 * src/local_filesystem rely on.
 * Build: gcc -shared -fPIC -O2 -o rename_noreplace.node rename_noreplace.c
 *        -I/usr/include/node
 */
#define _GNU_SOURCE /* struct ucred for SO_PEERCRED */
#include <node_api.h>
#include <fcntl.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>
#include <stdio.h>
#include <sys/statfs.h>
#include <sys/file.h>
#include <sys/socket.h>

#ifndef RENAME_NOREPLACE
#define RENAME_NOREPLACE (1U << 0)
#endif

/* Validate UTF-16 code units (reject lone surrogates that would alias
 * via U+FFFD) then copy the UTF-8 form; NULL out on rejection. */
static int get_path(napi_env env, napi_value arg, char *out, size_t cap) {
  char16_t u16[4096];
  size_t ulen = 0;
  if (napi_get_value_string_utf16(env, arg, NULL, 0, &ulen) != napi_ok ||
      ulen >= sizeof u16 / sizeof(char16_t) ||
      napi_get_value_string_utf16(env, arg, u16, sizeof u16 / sizeof(char16_t), &ulen) != napi_ok) {
    return 0;
  }
  for (size_t i = 0; i < ulen; i++) {
    char16_t c = u16[i];
    if (c >= 0xD800 && c <= 0xDBFF) {
      if (i + 1 >= ulen || u16[i + 1] < 0xDC00 || u16[i + 1] > 0xDFFF) return 0;
      i++;
    } else if (c >= 0xDC00 && c <= 0xDFFF) return 0;
  }
  size_t len = 0, n = 0;
  if (napi_get_value_string_utf8(env, arg, NULL, 0, &len) != napi_ok ||
      len >= cap ||
      napi_get_value_string_utf8(env, arg, out, cap, &n) != napi_ok ||
      n != len || strlen(out) != n) {
    return 0;
  }
  return 1;
}

/* openat(dirfd, name, O_RDONLY|O_DIRECTORY|O_NOFOLLOW) -> pinned dir fd */
static napi_value openat_dir(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  napi_get_cb_info(env, info, &argc, argv, NULL, NULL);
  int32_t dfd = -1;
  char name[4096];
  if (argc != 2 || napi_get_value_int32(env, argv[0], &dfd) != napi_ok ||
      dfd < 0 || !get_path(env, argv[1], name, sizeof name)) {
    napi_throw_type_error(env, "EINVAL", "openat_dir(dirfd, name)");
    return NULL;
  }
  int fd = openat(dfd, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
  if (fd < 0) {
    int e = errno;
    const char *code = e == ENOENT ? "ENOENT" : e == ENOTDIR ? "ENOTDIR"
      : e == ELOOP ? "ELOOP" : e == EACCES ? "EACCES" : "EIO";
    char msg[160];
    snprintf(msg, sizeof msg, "openat_dir failed: %s", strerror(e));
    napi_throw_error(env, code, msg);
    return NULL;
  }
  napi_value out;
  napi_create_int32(env, fd, &out);
  return out;
}

/* openat(dirfd, name, O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW, 0644) -> file fd */
static napi_value openat_create_excl(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  napi_get_cb_info(env, info, &argc, argv, NULL, NULL);
  int32_t dfd = -1;
  char name[4096];
  if (argc != 2 || napi_get_value_int32(env, argv[0], &dfd) != napi_ok ||
      dfd < 0 || !get_path(env, argv[1], name, sizeof name)) {
    napi_throw_type_error(env, "EINVAL", "openat_create_excl(dirfd, name)");
    return NULL;
  }
  int fd = openat(dfd, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0644);
  if (fd < 0) {
    int e = errno;
    const char *code = e == EEXIST ? "EEXIST" : e == ENOENT ? "ENOENT"
      : e == ENOTDIR ? "ENOTDIR" : e == ELOOP ? "ELOOP"
      : e == EACCES ? "EACCES" : "EIO";
    char msg[160];
    snprintf(msg, sizeof msg, "openat_create_excl failed: %s", strerror(e));
    napi_throw_error(env, code, msg);
    return NULL;
  }
  napi_value out;
  napi_create_int32(env, fd, &out);
  return out;
}

/* unlinkat(dirfd, name, 0) — unlink never follows the final symlink;
 * ancestor dirs pinned by the fd. EEXIST impossible; ENOENT/ENOTDIR
 * mapped for idempotent-remove callers. */
static napi_value unlinkat_file(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  napi_get_cb_info(env, info, &argc, argv, NULL, NULL);
  int32_t dfd = -1;
  char name[4096];
  if (argc != 2 || napi_get_value_int32(env, argv[0], &dfd) != napi_ok ||
      dfd < 0 || !get_path(env, argv[1], name, sizeof name)) {
    napi_throw_type_error(env, "EINVAL", "unlinkat_file(dirfd, name)");
    return NULL;
  }
  if (unlinkat(dfd, name, 0) < 0) {
    int e = errno;
    const char *code = e == ENOENT ? "ENOENT" : e == ENOTDIR ? "ENOTDIR"
      : e == EISDIR ? "EISDIR" : e == EACCES ? "EACCES" : "EIO";
    char msg[160];
    snprintf(msg, sizeof msg, "unlinkat failed: %s", strerror(e));
    napi_throw_error(env, code, msg);
    return NULL;
  }
  return NULL;
}

static napi_value rename_noreplace(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  napi_get_cb_info(env, info, &argc, argv, NULL, NULL);
  if (argc != 2) {
    napi_throw_type_error(env, "EINVAL", "rename_noreplace(src, dst) takes 2 args");
    return NULL;
  }
  char src[4096], dst[4096];
  /* get_path validates UTF-16 code units (lone surrogate -> reject,
   * no U+FFFD aliasing), bounds the UTF-8 length, verifies complete
   * copy and rejects embedded NUL — all before the syscall. */
  if (!get_path(env, argv[0], src, sizeof src) ||
      !get_path(env, argv[1], dst, sizeof dst)) {
    napi_throw_type_error(env, "EINVAL", "paths must be valid utf8/utf16 strings < 4096 bytes, no NUL");
    return NULL;
  }
  int rc = syscall(SYS_renameat2, AT_FDCWD, src, AT_FDCWD, dst, RENAME_NOREPLACE);
  if (rc != 0) {
    int e = errno;
    const char *code = e == EEXIST ? "EEXIST" : e == ENOENT ? "ENOENT"
      : e == ENOSYS ? "ENOSYS" : e == EINVAL ? "EINVAL" : "EIO";
    char msg[160];
    snprintf(msg, sizeof msg, "rename_noreplace failed: %s", strerror(e));
    napi_throw_error(env, code, msg);
    return NULL;
  }
  napi_value undef;
  napi_get_undefined(env, &undef);
  return undef;
}

/* flockfd(fd, op) -> 0 | errno — flock(LOCK_EX|LOCK_NB) parity with
   * the P1 linux_guards binding (verification runtime exclusion). */
static napi_value flockfd(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  if (napi_get_cb_info(env, info, &argc, argv, NULL, NULL) != napi_ok || argc < 2) {
    napi_throw_type_error(env, NULL, "flockfd expects (fd:number, operation:number)");
    return NULL;
  }
  int32_t fd, op;
  if (napi_get_value_int32(env, argv[0], &fd) != napi_ok
      || napi_get_value_int32(env, argv[1], &op) != napi_ok) {
    napi_throw_type_error(env, NULL, "fd and operation must be int32");
    return NULL;
  }
  errno = 0;
  int r = flock(fd, op);
  napi_value out;
  napi_create_int32(env, r == 0 ? 0 : errno, &out);
  return out;
}

/* statfsmagic(fd) -> f_type as a double — local-filesystem gate. */
static napi_value statfsmagic(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value argv[1];
  if (napi_get_cb_info(env, info, &argc, argv, NULL, NULL) != napi_ok || argc < 1) {
    napi_throw_type_error(env, NULL, "statfsmagic expects (fd:number)");
    return NULL;
  }
  int32_t fd;
  if (napi_get_value_int32(env, argv[0], &fd) != napi_ok) {
    napi_throw_type_error(env, NULL, "fd must be int32");
    return NULL;
  }
  struct statfs s;
  if (fstatfs(fd, &s) != 0) {
    int e = errno;
    napi_throw_error(env, NULL, "fstatfs failed");
    errno = e;
    return NULL;
  }
  napi_value out;
  napi_create_double(env, (double)s.f_type, &out);
  return out;
}

/* getpeereuid(fd) -> {pid, uid, gid} via getsockopt(SO_PEERCRED) —
 * peer() in src/local_runtime/paths.rs. Throws on non-socket fds and
 * on a truncated credential (same shape check as Rust). */
static napi_value getpeereuid(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value argv[1];
  if (napi_get_cb_info(env, info, &argc, argv, NULL, NULL) != napi_ok || argc < 1) {
    napi_throw_type_error(env, NULL, "getpeereuid expects (fd:number)");
    return NULL;
  }
  int32_t fd;
  if (napi_get_value_int32(env, argv[0], &fd) != napi_ok) {
    napi_throw_type_error(env, NULL, "fd must be an int32");
    return NULL;
  }
  struct ucred cred;
  socklen_t len = sizeof(cred);
  if (getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &cred, &len) != 0) {
    int e = errno;
    char msg[160];
    snprintf(msg, sizeof msg, "getsockopt(SO_PEERCRED) failed: %s", strerror(e));
    napi_throw_error(env, NULL, msg);
    return NULL;
  }
  if (len != sizeof(cred)) {
    napi_throw_error(env, NULL, "unexpected peer credential shape");
    return NULL;
  }
  napi_value obj, v;
  napi_create_object(env, &obj);
  napi_create_int32(env, cred.pid, &v); napi_set_named_property(env, obj, "pid", v);
  napi_create_uint32(env, cred.uid, &v); napi_set_named_property(env, obj, "uid", v);
  napi_create_uint32(env, cred.gid, &v); napi_set_named_property(env, obj, "gid", v);
  return obj;
}

static napi_value init(napi_env env, napi_value exports) {
  napi_value fn;
  napi_create_function(env, "rename_noreplace", NAPI_AUTO_LENGTH,
                       rename_noreplace, NULL, &fn);
  napi_set_named_property(env, exports, "rename_noreplace", fn);
  napi_create_function(env, "openat_dir", NAPI_AUTO_LENGTH,
                       openat_dir, NULL, &fn);
  napi_set_named_property(env, exports, "openat_dir", fn);
  napi_create_function(env, "openat_create_excl", NAPI_AUTO_LENGTH,
                       openat_create_excl, NULL, &fn);
  napi_set_named_property(env, exports, "openat_create_excl", fn);
  napi_create_function(env, "unlinkat_file", NAPI_AUTO_LENGTH,
                       unlinkat_file, NULL, &fn);
  napi_set_named_property(env, exports, "unlinkat_file", fn);
  napi_create_function(env, "flockfd", NAPI_AUTO_LENGTH,
                       flockfd, NULL, &fn);
  napi_set_named_property(env, exports, "flockfd", fn);
  napi_create_function(env, "statfsmagic", NAPI_AUTO_LENGTH,
                       statfsmagic, NULL, &fn);
  napi_set_named_property(env, exports, "statfsmagic", fn);
  napi_create_function(env, "getpeereuid", NAPI_AUTO_LENGTH,
                       getpeereuid, NULL, &fn);
  napi_set_named_property(env, exports, "getpeereuid", fn);
  return exports;
}
NAPI_MODULE(rename_noreplace, init)
