/* renameat2(RENAME_NOREPLACE) — atomic no-overwrite rename for immutable
 * generation install. Node fs.renameSync would silently replace an
 * existing empty target directory; this primitive fails EEXIST instead.
 * Build: gcc -shared -fPIC -O2 -o rename_noreplace.node rename_noreplace.c
 *        -I/usr/include/node
 */
#include <node_api.h>
#include <fcntl.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>
#include <stdio.h>

#ifndef RENAME_NOREPLACE
#define RENAME_NOREPLACE (1U << 0)
#endif

static napi_value rename_noreplace(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  napi_get_cb_info(env, info, &argc, argv, NULL, NULL);
  if (argc != 2) {
    napi_throw_type_error(env, "EINVAL", "rename_noreplace(src, dst) takes 2 args");
    return NULL;
  }
  char src[4096], dst[4096];
  size_t n1 = 0, n2 = 0;
  if (napi_get_value_string_utf8(env, argv[0], src, sizeof src, &n1) != napi_ok ||
      napi_get_value_string_utf8(env, argv[1], dst, sizeof dst, &n2) != napi_ok ||
      n1 >= sizeof src || n2 >= sizeof dst) {
    napi_throw_type_error(env, "EINVAL", "paths must be utf8 strings < 4096 bytes");
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

static napi_value init(napi_env env, napi_value exports) {
  napi_value fn;
  napi_create_function(env, "rename_noreplace", NAPI_AUTO_LENGTH,
                       rename_noreplace, NULL, &fn);
  napi_set_named_property(env, exports, "rename_noreplace", fn);
  return exports;
}
NAPI_MODULE(rename_noreplace, init)
