// zone-wayland: create a wp_security_context_v1 listener for one zone.
//
//   zone-wayland SOCKET_PATH APP_ID INSTANCE_ID
//
// Connects to the host compositor ($WAYLAND_DISPLAY), creates a listening
// unix socket at SOCKET_PATH and hands it to the compositor as a security
// context (engine "dev.mirql.zones"). Clients that connect to SOCKET_PATH are
// "sandboxed clients": Hyprland hides every privileged global from them.
// The context lives as long as this process: the close fd is the read end
// of a pipe whose write end only this process holds, so when it exits (or
// is killed with its zone) the compositor stops accepting on the socket.
// It prints "ready" on stdout once the context is committed.
// With $ZONE_WAYLAND_PARENT (the server's pid) it gets SIGTERM when that
// process dies (PR_SET_PDEATHSIG), and exits at once if it died already.
// Exits 3 when the compositor has no security-context protocol (the server
// then starts the zone without a Wayland socket), 1 on any other error.
// Same pattern as flatpak's common/flatpak-run-wayland.c.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <signal.h>
#include <wayland-client.h>
#include "security-context-v1-client-protocol.h"

static struct wp_security_context_manager_v1 *mgr;

static void global(void *data, struct wl_registry *reg, uint32_t name,
                   const char *iface, uint32_t version) {
  (void)data; (void)version;
  if (strcmp(iface, wp_security_context_manager_v1_interface.name) == 0)
    mgr = wl_registry_bind(reg, name, &wp_security_context_manager_v1_interface, 1);
}
static void global_remove(void *d, struct wl_registry *r, uint32_t n) { (void)d; (void)r; (void)n; }
static const struct wl_registry_listener reg_listener = { global, global_remove };

int main(int argc, char **argv) {
  if (argc != 4) {
    fprintf(stderr, "usage: zone-wayland SOCKET_PATH APP_ID INSTANCE_ID\n");
    return 2;
  }
  const char *parent = getenv("ZONE_WAYLAND_PARENT");
  if (parent) {
    // the server's death ends this helper; checked after prctl: no gap. SIGTERM
    // unblocked first, in case whoever started the server had it blocked
    sigset_t term;
    sigemptyset(&term);
    sigaddset(&term, SIGTERM);
    sigprocmask(SIG_UNBLOCK, &term, NULL);
    if (prctl(PR_SET_PDEATHSIG, SIGTERM) < 0) { perror("prctl"); return 1; }
    if (getppid() != (pid_t)atol(parent)) { fprintf(stderr, "zone-wayland: server gone\n"); return 1; }
  }
  const char *path = argv[1];
  struct wl_display *dpy = wl_display_connect(NULL);
  if (!dpy) { perror("wl_display_connect"); return 1; }
  struct wl_registry *reg = wl_display_get_registry(dpy);
  wl_registry_add_listener(reg, &reg_listener, NULL);
  wl_display_roundtrip(dpy);
  if (!mgr) { fprintf(stderr, "compositor has no wp_security_context_manager_v1\n"); return 3; }

  int lfd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  struct sockaddr_un sa = { .sun_family = AF_UNIX };
  if (strlen(path) >= sizeof sa.sun_path) { fprintf(stderr, "path too long\n"); return 1; }
  strcpy(sa.sun_path, path);
  unlink(path);
  if (bind(lfd, (struct sockaddr *)&sa, sizeof sa) < 0 || listen(lfd, 16) < 0) {
    perror("bind/listen"); return 1;
  }
  int p[2];
  if (pipe2(p, O_CLOEXEC) < 0) { perror("pipe"); return 1; }

  struct wp_security_context_v1 *ctx = wp_security_context_manager_v1_create_listener(mgr, lfd, p[0]);
  close(lfd); close(p[0]);
  wp_security_context_v1_set_sandbox_engine(ctx, "dev.mirql.zones");
  wp_security_context_v1_set_app_id(ctx, argv[2]);
  wp_security_context_v1_set_instance_id(ctx, argv[3]);
  wp_security_context_v1_commit(ctx);
  wp_security_context_v1_destroy(ctx);
  if (wl_display_roundtrip(dpy) < 0) { fprintf(stderr, "protocol error: %d\n", wl_display_get_error(dpy)); return 1; }
  printf("ready\n"); fflush(stdout);

  // Hold p[1] until we are told to stop or the compositor goes away.
  while (wl_display_dispatch(dpy) != -1) {}
  return 0;
}
