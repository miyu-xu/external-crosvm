// vmnet_shim.c — Bridges vmnet.framework block-based callbacks to plain C.
// Compile with: -F/System/Library/Frameworks -framework vmnet
#include "vmnet_shim.h"
#include <vmnet/vmnet.h>
#include <dispatch/dispatch.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>
#include <unistd.h>

struct vmnet_shim_interface {
    interface_ref vmnet_iface;
    dispatch_queue_t queue;
    int notify_fd;
    size_t max_packet_size;
    char mac_address[18];
    int start_status;
};

vmnet_shim_t *vmnet_shim_start(uint32_t mode,
                               const char *mac_addr,
                               uint64_t mtu,
                               int notify_fd,
                               int *error_code)
{
    struct vmnet_shim_interface *iface = calloc(1, sizeof(*iface));
    if (!iface) {
        if (error_code) *error_code = VMNET_MEM_FAILURE;
        return NULL;
    }
    iface->notify_fd = notify_fd;
    iface->queue = dispatch_queue_create("com.bscp.vmnet",
                                         DISPATCH_QUEUE_SERIAL);

    xpc_object_t desc = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_uint64(desc, vmnet_operation_mode_key, mode);
    if (mac_addr) {
        xpc_dictionary_set_string(desc, vmnet_mac_address_key, mac_addr);
    }
    if (mtu > 0) {
        xpc_dictionary_set_uint64(desc, vmnet_mtu_key, mtu);
    }

    dispatch_semaphore_t sem = dispatch_semaphore_create(0);

    interface_ref viface = vmnet_start_interface(
        desc,
        iface->queue,
        ^(vmnet_return_t status, xpc_object_t __nullable param) {
            iface->start_status = (int)status;
            if (status == VMNET_SUCCESS && param) {
                iface->max_packet_size =
                    xpc_dictionary_get_uint64(param,
                                              vmnet_max_packet_size_key);
                const char *m =
                    xpc_dictionary_get_string(param, vmnet_mac_address_key);
                if (m) {
                    strncpy(iface->mac_address, m,
                            sizeof(iface->mac_address) - 1);
                }
            }
            dispatch_semaphore_signal(sem);
        });

    xpc_release(desc);

    if (!viface) {
        if (error_code) *error_code = VMNET_FAILURE;
        dispatch_release(iface->queue);
        free(iface);
        dispatch_release(sem);
        return NULL;
    }
    iface->vmnet_iface = viface;

    // Wait for the start completion.
    dispatch_semaphore_wait(sem, DISPATCH_TIME_FOREVER);
    dispatch_release(sem);

    if (iface->start_status != (int)VMNET_SUCCESS) {
        if (error_code) *error_code = iface->start_status;
        dispatch_release(iface->queue);
        free(iface);
        return NULL;
    }

    // Register the "packets available" callback.
    vmnet_interface_set_event_callback(
        viface,
        VMNET_INTERFACE_PACKETS_AVAILABLE,
        iface->queue,
        ^(interface_event_t event_mask, xpc_object_t __unused event) {
            if (event_mask & VMNET_INTERFACE_PACKETS_AVAILABLE) {
                char c = 1;
                // Best-effort write — if the pipe is full the Rust side
                // already knows there is work to do.
                (void)write(iface->notify_fd, &c, 1);
            }
        });

    if (error_code) *error_code = (int)VMNET_SUCCESS;
    return iface;
}

int vmnet_shim_read(vmnet_shim_t *iface,
                    void *buf, size_t buf_size,
                    size_t *bytes_read)
{
    *bytes_read = 0;
    if (!iface || !iface->vmnet_iface) return VMNET_INVALID_ARGUMENT;

    struct iovec iov = { .iov_base = buf, .iov_len = buf_size };
    struct vmpktdesc pkt = {
        .vm_pkt_size = buf_size,
        .vm_pkt_iov  = &iov,
        .vm_pkt_iovcnt = 1,
        .vm_flags    = 0,
    };
    int pktcnt = 1;
    vmnet_return_t ret = vmnet_read(iface->vmnet_iface, &pkt, &pktcnt);
    if (ret == VMNET_SUCCESS && pktcnt > 0) {
        *bytes_read = pkt.vm_pkt_size;
    }
    return (int)ret;
}

int vmnet_shim_write(vmnet_shim_t *iface, const void *buf, size_t size)
{
    if (!iface || !iface->vmnet_iface) return VMNET_INVALID_ARGUMENT;

    struct iovec iov = { .iov_base = (void *)buf, .iov_len = size };
    struct vmpktdesc pkt = {
        .vm_pkt_size   = size,
        .vm_pkt_iov    = &iov,
        .vm_pkt_iovcnt = 1,
        .vm_flags      = 0,
    };
    int pktcnt = 1;
    return (int)vmnet_write(iface->vmnet_iface, &pkt, &pktcnt);
}

size_t vmnet_shim_max_packet_size(vmnet_shim_t *iface)
{
    return iface ? iface->max_packet_size : 0;
}

const char *vmnet_shim_mac_address(vmnet_shim_t *iface)
{
    if (!iface || iface->mac_address[0] == '\0') return NULL;
    return iface->mac_address;
}

void vmnet_shim_stop(vmnet_shim_t *iface)
{
    if (!iface) return;
    if (iface->vmnet_iface) {
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);
        vmnet_stop_interface(iface->vmnet_iface, iface->queue,
                             ^(vmnet_return_t _status) {
            dispatch_semaphore_signal(sem);
        });
        dispatch_semaphore_wait(sem, DISPATCH_TIME_FOREVER);
        dispatch_release(sem);
        iface->vmnet_iface = NULL;
    }
    if (iface->queue) {
        dispatch_release(iface->queue);
        iface->queue = NULL;
    }
    free(iface);
}
