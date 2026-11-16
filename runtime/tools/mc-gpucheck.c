/*
 * mc-gpucheck — is the GPU's kernel driver the one the runtime's Mesa speaks?
 *
 * Asks each render node for its DRM driver name and version and compares them
 * with /etc/openhc-minecraft-gpu ("DRIVER UAPI_MAJOR", written when the runtime
 * is built). Mesa itself refuses a different uAPI major; this says so before the
 * game starts instead of after. Exit 0 when one node matches.
 */
#include <dirent.h>
#include <drm/drm.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

int main(void)
{
	char want[64] = "", path[300], name[64];
	int major = -1, found = 0;
	FILE *f = fopen("/etc/openhc-minecraft-gpu", "r");
	if (!f || fscanf(f, "%63s %d", want, &major) != 2) {
		fprintf(stderr, "mc-gpucheck: no /etc/openhc-minecraft-gpu\n");
		return 2;
	}
	fclose(f);
	DIR *d = opendir("/dev/dri");
	struct dirent *e;
	while (d && (e = readdir(d))) {
		if (strncmp(e->d_name, "renderD", 7))
			continue;
		snprintf(path, sizeof path, "/dev/dri/%s", e->d_name);
		int fd = open(path, O_RDWR | O_CLOEXEC);
		if (fd < 0)
			continue;
		struct drm_version v = { 0 };
		v.name = name;
		v.name_len = sizeof name - 1;
		if (ioctl(fd, DRM_IOCTL_VERSION, &v) == 0) {
			name[v.name_len < sizeof name ? v.name_len : sizeof name - 1] = 0;
			if (!strcmp(name, want)) {
				found = 1;
				if (v.version_major == major) {
					close(fd);
					return 0;
				}
				fprintf(stderr, "mc-gpucheck: %s uAPI %d.%d on %s, the runtime's Mesa needs %d.x\n",
					name, v.version_major, v.version_minor, path, major);
			}
		}
		close(fd);
	}
	if (!found)
		fprintf(stderr, "mc-gpucheck: no %s render node\n", want);
	return 1;
}
