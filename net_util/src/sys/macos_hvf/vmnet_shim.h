// vmnet_shim.h — C bridge from vmnet.framework (block-based API) to plain C.
// Rust code calls these extern "C" functions; the .c file handles the blocks.
#ifndef VMNET_SHIM_H_
#define VMNET_SHIM_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Opaque handle.
typedef struct vmnet_shim_interface vmnet_shim_t;

// Create and start a vmnet interface.  mode is one of the operating_modes_t
// constants (1000 = host, 1001 = shared/NAT, 1002 = bridged).
// mac_addr may be NULL (auto-assign).  mtu may be 0 (default).
// notify_fd is the write-end of a pipe — one byte is written whenever packets
// are available to read.
// On failure returns NULL and sets *error_code (if non-NULL) to the vmnet_return_t.
vmnet_shim_t *vmnet_shim_start(uint32_t mode,
                               const char *mac_addr,
                               uint64_t mtu,
                               int notify_fd,
                               int *error_code);

// Read one packet.  Blocks until a packet is available.
// Returns 1000 (VMNET_SUCCESS) on success; *bytes_read is the actual size.
// Returns another vmnet_return_t code on error.
int vmnet_shim_read(vmnet_shim_t *iface,
                    void *buf, size_t buf_size,
                    size_t *bytes_read);

// Write one packet.
// Returns 1000 (VMNET_SUCCESS) on success, another vmnet_return_t on error.
int vmnet_shim_write(vmnet_shim_t *iface, const void *buf, size_t size);

// Maximum packet size the interface was configured with.
size_t vmnet_shim_max_packet_size(vmnet_shim_t *iface);

// Assigned MAC address ("xx:xx:xx:xx:xx:xx"), or NULL if unknown.
const char *vmnet_shim_mac_address(vmnet_shim_t *iface);

// Stop and destroy the interface.  Safe to call with NULL.
void vmnet_shim_stop(vmnet_shim_t *iface);

#ifdef __cplusplus
}
#endif

#endif // VMNET_SHIM_H_
