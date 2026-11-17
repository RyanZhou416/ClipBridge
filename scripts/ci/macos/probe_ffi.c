/* Minimal, side-effect-free FFI probe used by scripts/ci/macos.sh.
 *
 * It only dlopen()s the dylib that was embedded into the built .app, resolves
 * cb_get_ffi_version and checks the ABI major the shell expects. It does not
 * call cb_init, start the app/UI, touch the network or read user data.
 *
 * Exit code 0 = dylib loaded, symbol resolved, ABI major compatible.
 */
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>

typedef void (*cb_get_ffi_version_fn)(uint32_t *out_major, uint32_t *out_minor);

#define EXPECTED_FFI_ABI_MAJOR 1u

int main(int argc, char **argv) {
	if (argc != 2) {
		fprintf(stderr, "usage: %s <libcore_ffi_macos.dylib>\n", argv[0]);
		return 2;
	}

	void *handle = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
	if (handle == NULL) {
		fprintf(stderr, "dlopen failed: %s\n", dlerror());
		return 1;
	}

	dlerror();
	cb_get_ffi_version_fn get_version = (cb_get_ffi_version_fn)dlsym(handle, "cb_get_ffi_version");
	if (get_version == NULL) {
		const char *error = dlerror();
		fprintf(stderr, "missing cb_get_ffi_version: %s\n", error != NULL ? error : "unknown");
		dlclose(handle);
		return 1;
	}

	uint32_t major = 0xFFFFFFFFu;
	uint32_t minor = 0xFFFFFFFFu;
	get_version(&major, &minor);

	int rc = 0;
	if (major != EXPECTED_FFI_ABI_MAJOR) {
		fprintf(stderr, "incompatible FFI ABI major %u (expected %u)\n", major, EXPECTED_FFI_ABI_MAJOR);
		rc = 1;
	}

	if (dlclose(handle) != 0) {
		fprintf(stderr, "dlclose failed: %s\n", dlerror());
		rc = 1;
	}

	if (rc != 0) {
		return rc;
	}
	printf("cb_get_ffi_version=%u.%u\n", major, minor);
	return 0;
}
