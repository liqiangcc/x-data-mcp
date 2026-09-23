// NM-009: smallest real N-API binding for Linux credential/lease primitives.
// getpeereuid(fd)  -> {pid, uid, gid} via getsockopt(SO_PEERCRED) — mirrors
//                     src/local_runtime/paths.rs::peer()
// flockfd(fd, op)  -> 0 on success, else errno — mirrors fs2::try_lock
//                     (flock LOCK_EX|LOCK_NB) in src/local_runtime/paths.rs
// Both operate on real fds owned by the Node process — no subprocess.
#define _GNU_SOURCE
#include <errno.h>
#include <node_api.h>
#include <sys/file.h>
#include <sys/socket.h>
#include <unistd.h>

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
    napi_throw_error(env, NULL, "getsockopt(SO_PEERCRED) failed");
    return NULL;
  }
  if (len != sizeof(cred)) { // same shape check as Rust
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

NAPI_MODULE_INIT() {
  napi_value fn;
  napi_create_function(env, "getpeereuid", NAPI_AUTO_LENGTH, getpeereuid, NULL, &fn);
  napi_set_named_property(env, exports, "getpeereuid", fn);
  napi_create_function(env, "flockfd", NAPI_AUTO_LENGTH, flockfd, NULL, &fn);
  napi_set_named_property(env, exports, "flockfd", fn);
  return exports;
}
