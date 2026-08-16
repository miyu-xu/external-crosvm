// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#import <Cocoa/Cocoa.h>
#import <QuartzCore/QuartzCore.h>
#import <dispatch/dispatch.h>
#import <os/lock.h>
#import <os/log.h>

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

enum {
    CROSVM_COCOA_EVENT_KEY = 1,
    CROSVM_COCOA_EVENT_TOUCH_DOWN = 2,
    CROSVM_COCOA_EVENT_TOUCH_MOVE = 3,
    CROSVM_COCOA_EVENT_TOUCH_UP = 4,
    CROSVM_COCOA_EVENT_VIEWPORT_RESIZE = 5,
    CROSVM_COCOA_EVENT_SELECT_DISPLAY = 6,
};

typedef struct {
    int32_t kind;
    int32_t code;
    int32_t value;
    int32_t repeat;
    int32_t x;
    int32_t y;
} CrosvmCocoaInputEvent;

typedef struct {
    uint32_t magic;
    uint32_t context_id;
    uint32_t guest_width;
    uint32_t guest_height;
} CrosvmCaHello;

extern void gfxstream_backend_select_display(uint32_t display_id);

@interface CAContext : NSObject
+ (instancetype)contextWithCGSConnection:(uint32_t)connection
                                 options:(NSDictionary*)options;
@property(nonatomic, strong) CALayer* layer;
@property(nonatomic, readonly) uint32_t contextId;
@end

#define CROSVM_COCOA_EVENT_QUEUE_CAPACITY 256

static NSWindow* gCrosvmWindow = nil;
static id gCrosvmInputMonitor = nil;
static CAContext* gRemoteContext = nil;
static CALayer* gRemoteRootLayer = nil;
static CALayer* gRemoteContentLayer = nil;
static CALayer* gRemoteRenderLayer = nil;
static int gRemoteSocket = -1;
static uint32_t gGuestWidth = 1;
static uint32_t gGuestHeight = 1;
static int gInputPipe[2] = {-1, -1};
static dispatch_once_t gInputPipeOnce;
static os_unfair_lock gInputLock = OS_UNFAIR_LOCK_INIT;
static CrosvmCocoaInputEvent
    gInputQueue[CROSVM_COCOA_EVENT_QUEUE_CAPACITY];
static size_t gInputQueueHead = 0;
static size_t gInputQueueTail = 0;
static size_t gInputQueueCount = 0;

static void crosvm_cocoa_initialize_input_pipe(void) {
    dispatch_once(&gInputPipeOnce, ^{
        if (pipe(gInputPipe) != 0) {
            gInputPipe[0] = -1;
            gInputPipe[1] = -1;
            return;
        }
        for (int i = 0; i < 2; ++i) {
            int flags = fcntl(gInputPipe[i], F_GETFL);
            if (flags >= 0) {
                (void)fcntl(gInputPipe[i], F_SETFL, flags | O_NONBLOCK);
            }
            int fd_flags = fcntl(gInputPipe[i], F_GETFD);
            if (fd_flags >= 0) {
                (void)fcntl(gInputPipe[i], F_SETFD, fd_flags | FD_CLOEXEC);
            }
        }
    });
}

static void crosvm_cocoa_enqueue_input(CrosvmCocoaInputEvent event) {
    crosvm_cocoa_initialize_input_pipe();
    if (gInputPipe[1] < 0) {
        return;
    }

    os_unfair_lock_lock(&gInputLock);
    if (gInputQueueCount == CROSVM_COCOA_EVENT_QUEUE_CAPACITY) {
        os_unfair_lock_unlock(&gInputLock);
        return;
    }
    gInputQueue[gInputQueueTail] = event;
    gInputQueueTail =
        (gInputQueueTail + 1) % CROSVM_COCOA_EVENT_QUEUE_CAPACITY;
    ++gInputQueueCount;
    uint8_t byte = 1;
    if (write(gInputPipe[1], &byte, sizeof(byte)) != sizeof(byte)) {
        gInputQueueTail =
            (gInputQueueTail + CROSVM_COCOA_EVENT_QUEUE_CAPACITY - 1) %
            CROSVM_COCOA_EVENT_QUEUE_CAPACITY;
        --gInputQueueCount;
    }
    os_unfair_lock_unlock(&gInputLock);
}

int crosvm_cocoa_event_read_fd(void) {
    crosvm_cocoa_initialize_input_pipe();
    return gInputPipe[0] < 0 ? -1 : dup(gInputPipe[0]);
}

int32_t crosvm_cocoa_pending_event(void) {
    os_unfair_lock_lock(&gInputLock);
    int32_t pending = gInputQueueCount != 0;
    os_unfair_lock_unlock(&gInputLock);
    return pending;
}

int32_t crosvm_cocoa_next_event(CrosvmCocoaInputEvent* event) {
    if (event == NULL) {
        return 0;
    }
    os_unfair_lock_lock(&gInputLock);
    if (gInputQueueCount == 0) {
        os_unfair_lock_unlock(&gInputLock);
        return 0;
    }
    *event = gInputQueue[gInputQueueHead];
    gInputQueueHead =
        (gInputQueueHead + 1) % CROSVM_COCOA_EVENT_QUEUE_CAPACITY;
    --gInputQueueCount;
    uint8_t byte;
    (void)read(gInputPipe[0], &byte, sizeof(byte));
    os_unfair_lock_unlock(&gInputLock);
    return 1;
}

static int32_t crosvm_cocoa_modifier_pressed(NSEvent* event) {
    NSEventModifierFlags flags = [event modifierFlags];
    switch ([event keyCode]) {
        case 54:
        case 55:
            return (flags & NSEventModifierFlagCommand) != 0;
        case 56:
        case 60:
            return (flags & NSEventModifierFlagShift) != 0;
        case 57:
            return (flags & NSEventModifierFlagCapsLock) != 0;
        case 58:
        case 61:
            return (flags & NSEventModifierFlagOption) != 0;
        case 59:
        case 62:
            return (flags & NSEventModifierFlagControl) != 0;
        default:
            return 0;
    }
}

static void crosvm_cocoa_install_input_monitor(void) {
    if (gCrosvmInputMonitor != nil) {
        return;
    }
    NSEventMask mask = NSEventMaskKeyDown | NSEventMaskKeyUp |
                       NSEventMaskFlagsChanged | NSEventMaskLeftMouseDown |
                       NSEventMaskLeftMouseUp | NSEventMaskLeftMouseDragged;
    gCrosvmInputMonitor =
        [NSEvent addLocalMonitorForEventsMatchingMask:mask
                                               handler:^NSEvent*(NSEvent* event) {
        if ([event window] != gCrosvmWindow) {
            return event;
        }

        CrosvmCocoaInputEvent input = {0};
        switch ([event type]) {
            case NSEventTypeKeyDown:
            case NSEventTypeKeyUp:
                input.kind = CROSVM_COCOA_EVENT_KEY;
                input.code = [event keyCode];
                input.value = [event type] == NSEventTypeKeyDown;
                input.repeat = [event isARepeat];
                break;
            case NSEventTypeFlagsChanged:
                input.kind = CROSVM_COCOA_EVENT_KEY;
                input.code = [event keyCode];
                input.value = crosvm_cocoa_modifier_pressed(event);
                break;
            case NSEventTypeLeftMouseDown:
            case NSEventTypeLeftMouseDragged:
            case NSEventTypeLeftMouseUp: {
                NSView* view = [gCrosvmWindow contentView];
                NSPoint point = [view convertPoint:[event locationInWindow]
                                          fromView:nil];
                NSRect bounds = [view bounds];
                double width = MAX(bounds.size.width, 1.0);
                double height = MAX(bounds.size.height, 1.0);
                input.x = (int32_t)MAX(
                    0.0, MIN((double)gGuestWidth - 1.0,
                             point.x * gGuestWidth / width));
                input.y = (int32_t)MAX(
                    0.0, MIN((double)gGuestHeight - 1.0,
                             (height - point.y) * gGuestHeight / height));
                input.kind = [event type] == NSEventTypeLeftMouseDown
                                 ? CROSVM_COCOA_EVENT_TOUCH_DOWN
                             : [event type] == NSEventTypeLeftMouseUp
                                 ? CROSVM_COCOA_EVENT_TOUCH_UP
                                 : CROSVM_COCOA_EVENT_TOUCH_MOVE;
                break;
            }
            default:
                return event;
        }
        crosvm_cocoa_enqueue_input(input);
        return event;
    }];
}

void crosvm_cocoa_run_main_loop(void) {
    @autoreleasepool {
        NSApplication* app = [NSApplication sharedApplication];
        [app setActivationPolicy:getenv("CROSVM_COCOA_CONTEXT_ENDPOINT") == NULL
                                     ? NSApplicationActivationPolicyRegular
                                     : NSApplicationActivationPolicyAccessory];
        [app finishLaunching];
        [app run];
    }
}

void crosvm_cocoa_stop_main_loop(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSApp stop:nil];
        NSEvent* wake =
            [NSEvent otherEventWithType:NSEventTypeApplicationDefined
                               location:NSZeroPoint
                          modifierFlags:0
                              timestamp:0
                           windowNumber:0
                                context:nil
                                subtype:0
                                  data1:0
                                  data2:0];
        [NSApp postEvent:wake atStart:NO];
    });
}

void crosvm_cocoa_run_on_main(void (*callback)(void*), void* context) {
    if (callback == NULL) {
        return;
    }
    void (^invoke)(void) = ^{ callback(context); };
    if ([NSThread isMainThread]) {
        invoke();
    } else {
        dispatch_sync(dispatch_get_main_queue(), invoke);
    }
}

void* crosvm_cocoa_create_window(uint32_t width, uint32_t height) {
    __block NSWindow* result = nil;
    void (^createWindow)(void) = ^{
        gGuestWidth = MAX(width, 1);
        gGuestHeight = MAX(height, 1);
        if (gCrosvmWindow == nil) {
            NSRect rect = NSMakeRect(0, 0, width, height);
            NSWindowStyleMask style =
                NSWindowStyleMaskTitled |
                NSWindowStyleMaskClosable |
                NSWindowStyleMaskMiniaturizable |
                NSWindowStyleMaskResizable;
            gCrosvmWindow =
                [[NSWindow alloc] initWithContentRect:rect
                                           styleMask:style
                                             backing:NSBackingStoreBuffered
                                               defer:NO];
            [gCrosvmWindow setTitle:@"crosvm Android"];
            [gCrosvmWindow setReleasedWhenClosed:NO];
            [gCrosvmWindow center];
        } else {
            [gCrosvmWindow setContentSize:NSMakeSize(width, height)];
        }
        [gCrosvmWindow setAcceptsMouseMovedEvents:YES];
        if (getenv("CROSVM_COCOA_CONTEXT_ENDPOINT") == NULL) {
            [gCrosvmWindow makeKeyAndOrderFront:nil];
            [NSApp activateIgnoringOtherApps:YES];
            crosvm_cocoa_install_input_monitor();
        } else {
            [gCrosvmWindow orderFront:nil];
        }
        result = gCrosvmWindow;
    };

    if ([NSThread isMainThread]) {
        createWindow();
    } else {
        dispatch_sync(dispatch_get_main_queue(), createWindow);
    }
    return (__bridge void*)result;
}

static CALayer* crosvm_cocoa_find_metal_layer(NSView* view) {
    CALayer* layer = view.layer;
    Class metalLayerClass = NSClassFromString(@"CAMetalLayer");
    if (layer != nil && metalLayerClass != Nil && [layer isKindOfClass:metalLayerClass]) {
        return layer;
    }
    for (NSView* subview in view.subviews.reverseObjectEnumerator) {
        CALayer* candidate = crosvm_cocoa_find_metal_layer(subview);
        if (candidate != nil) {
            return candidate;
        }
    }
    return nil;
}

static void crosvm_cocoa_resize_remote_layer(uint32_t viewportWidth,
                                               uint32_t viewportHeight) {
    if (gRemoteRootLayer == nil || gRemoteContentLayer == nil ||
        gRemoteRenderLayer == nil) {
        return;
    }
    CGFloat width = MAX((CGFloat)viewportWidth, 1.0);
    CGFloat height = MAX((CGFloat)viewportHeight, 1.0);
    CGFloat guestWidth = MAX((CGFloat)gGuestWidth, 1.0);
    CGFloat guestHeight = MAX((CGFloat)gGuestHeight, 1.0);
    CGFloat fit = MIN(width / guestWidth, height / guestHeight);
    CGFloat renderWidth = MAX(guestWidth * fit, 1.0);
    CGFloat renderHeight = MAX(guestHeight * fit, 1.0);
    CGFloat renderX = floor((width - renderWidth) / 2.0);
    CGFloat renderY = floor((height - renderHeight) / 2.0);
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    gRemoteRootLayer.bounds = CGRectMake(0.0, 0.0, width, height);
    gRemoteRootLayer.frame = gRemoteRootLayer.bounds;
    gRemoteRootLayer.sublayerTransform = CATransform3DIdentity;
    gRemoteContentLayer.bounds = gRemoteRootLayer.bounds;
    gRemoteContentLayer.position = CGPointMake(0.0, 0.0);
    gRemoteContentLayer.affineTransform = CGAffineTransformIdentity;
    // A remote CAMetalLayer is composited as its own surface. WindowServer keeps
    // ancestor translations but does not reliably apply ancestor scale transforms
    // to that surface. Size the presentation layer directly while retaining a
    // guest-sized Metal drawable, so scaling remains a zero-copy GPU composition.
    gRemoteRenderLayer.bounds = CGRectMake(0.0, 0.0, renderWidth, renderHeight);
    gRemoteRenderLayer.position = CGPointMake(renderX, renderY);
    gRemoteRenderLayer.affineTransform = CGAffineTransformIdentity;
    gRemoteRenderLayer.contentsScale = 1.0 / fit;
    if ([gRemoteRenderLayer isKindOfClass:[CAMetalLayer class]]) {
        ((CAMetalLayer*)gRemoteRenderLayer).drawableSize =
            CGSizeMake(guestWidth, guestHeight);
    }
    [CATransaction commit];
    [CATransaction flush];
}

static bool crosvm_cocoa_read_full(int fd, void* buffer, size_t length) {
    uint8_t* cursor = buffer;
    while (length != 0) {
        ssize_t count = read(fd, cursor, length);
        if (count == 0) {
            return false;
        }
        if (count < 0) {
            if (errno == EINTR) {
                continue;
            }
            return false;
        }
        cursor += count;
        length -= (size_t)count;
    }
    return true;
}

int32_t crosvm_cocoa_publish_remote_layer(const char* endpoint,
                                           uint32_t width,
                                           uint32_t height) {
    if (endpoint == NULL || endpoint[0] == '\0') {
        return 0;
    }
    __block int32_t succeeded = 0;
    void (^publish)(void) = ^{
        if (gRemoteSocket >= 0 && gRemoteContext != nil) {
            succeeded = 1;
            return;
        }
        NSArray<NSWindow*>* childWindows = gCrosvmWindow.childWindows;
        CALayer* renderLayer = gRemoteRenderLayer;
        if (renderLayer == nil) {
            renderLayer = crosvm_cocoa_find_metal_layer(gCrosvmWindow.contentView);
        }
        if (renderLayer == nil) {
            for (NSWindow* child in childWindows.reverseObjectEnumerator) {
                renderLayer = crosvm_cocoa_find_metal_layer(child.contentView);
                if (renderLayer != nil) {
                    break;
                }
            }
        }
        if (renderLayer == nil) {
            return;
        }
        typedef uint32_t (*CGSMainConnectionIDFn)(void);
        CGSMainConnectionIDFn connection =
            (CGSMainConnectionIDFn)dlsym(RTLD_DEFAULT, "CGSMainConnectionID");
        Class contextClass = NSClassFromString(@"CAContext");
        if (connection == NULL || contextClass == Nil) {
            return;
        }
        CAContext* context =
            [contextClass contextWithCGSConnection:connection() options:@{}];
        if (context == nil) {
            return;
        }
        CALayer* rootLayer = [CALayer layer];
        rootLayer.anchorPoint = CGPointMake(0.0, 0.0);
        rootLayer.position = CGPointMake(0.0, 0.0);
        rootLayer.bounds =
            CGRectMake(0.0, 0.0, MAX(width, 1), MAX(height, 1));
        rootLayer.frame = rootLayer.bounds;
        rootLayer.sublayerTransform = CATransform3DIdentity;
        rootLayer.geometryFlipped = NO;
        rootLayer.opaque = YES;
        rootLayer.backgroundColor = NSColor.blackColor.CGColor;
        rootLayer.contentsScale = MAX(renderLayer.contentsScale, 1.0);
        CALayer* contentLayer = [CALayer layer];
        contentLayer.anchorPoint = CGPointMake(0.0, 0.0);
        contentLayer.position = CGPointMake(0.0, 0.0);
        contentLayer.bounds = rootLayer.bounds;
        contentLayer.frame = contentLayer.bounds;
        contentLayer.masksToBounds = YES;

        // CAContext expects a stable layer tree root. A CAMetalLayer used directly as the
        // context root can produce one drawable and then become detached when AppKit tears
        // down its backing NSView. Move it under a context-owned root before hiding the
        // source window so Metal can keep presenting to CALayerHost without pixel readback.
        [CATransaction begin];
        [CATransaction setDisableActions:YES];
        id renderDelegate = renderLayer.delegate;
        if ([renderDelegate isKindOfClass:[NSView class]]) {
            [(NSView*)renderDelegate removeFromSuperview];
        }
        [renderLayer removeFromSuperlayer];
        renderLayer.delegate = nil;
        renderLayer.hidden = NO;
        renderLayer.anchorPoint = CGPointMake(0.0, 0.0);
        renderLayer.position = CGPointMake(0.0, 0.0);
        renderLayer.bounds = rootLayer.bounds;
        renderLayer.affineTransform = CGAffineTransformIdentity;
        renderLayer.autoresizingMask = kCALayerNotSizable;
        [contentLayer addSublayer:renderLayer];
        [rootLayer addSublayer:contentLayer];
        context.layer = rootLayer;
        [CATransaction commit];
        [CATransaction flush];
        uint32_t contextId = context.contextId;
        if (contextId == 0) {
            return;
        }

        int fd = socket(AF_UNIX, SOCK_STREAM, 0);
        if (fd < 0) {
            return;
        }
        int closeOnExec = fcntl(fd, F_GETFD);
        if (closeOnExec >= 0) {
            (void)fcntl(fd, F_SETFD, closeOnExec | FD_CLOEXEC);
        }
        struct sockaddr_un address = {0};
        address.sun_family = AF_UNIX;
        if (strlen(endpoint) >= sizeof(address.sun_path)) {
            close(fd);
            return;
        }
        strlcpy(address.sun_path, endpoint, sizeof(address.sun_path));
        if (connect(fd, (const struct sockaddr*)&address, sizeof(address)) != 0) {
            close(fd);
            return;
        }
        int noSigPipe = 1;
        (void)setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigPipe, sizeof(noSigPipe));
        CrosvmCaHello hello = {
            .magic = 0x48444341,
            .context_id = contextId,
            .guest_width = MAX(width, 1),
            .guest_height = MAX(height, 1),
        };
        if (write(fd, &hello, sizeof(hello)) != sizeof(hello)) {
            close(fd);
            return;
        }
        gRemoteRenderLayer = renderLayer;
        gRemoteContentLayer = contentLayer;
        gRemoteRootLayer = rootLayer;
        gRemoteContext = context;
        gRemoteSocket = fd;
        [gCrosvmWindow orderOut:nil];
        for (NSWindow* child in childWindows) {
            [child orderOut:nil];
        }
        [CATransaction flush];
        succeeded = 1;
        dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE, 0), ^{
            while (true) {
                CrosvmCocoaInputEvent event = {0};
                if (!crosvm_cocoa_read_full(fd, &event, sizeof(event))) {
                    break;
                }
                if (event.kind == CROSVM_COCOA_EVENT_SELECT_DISPLAY) {
                    gfxstream_backend_select_display((uint32_t)MAX(event.code, 0));
                    crosvm_cocoa_enqueue_input(event);
                    continue;
                }
                if (event.kind == CROSVM_COCOA_EVENT_VIEWPORT_RESIZE) {
                    uint32_t viewportWidth = (uint32_t)MAX(event.x, 1);
                    uint32_t viewportHeight = (uint32_t)MAX(event.y, 1);
                    dispatch_async(dispatch_get_main_queue(), ^{
                        crosvm_cocoa_resize_remote_layer(viewportWidth, viewportHeight);
                    });
                    continue;
                }
                crosvm_cocoa_enqueue_input(event);
            }
            close(fd);
            dispatch_async(dispatch_get_main_queue(), ^{
                if (gRemoteSocket == fd) {
                    gRemoteSocket = -1;
                    gRemoteContext = nil;
                    gRemoteRootLayer = nil;
                    gRemoteContentLayer = nil;
                }
            });
        });
    };
    if ([NSThread isMainThread]) {
        publish();
    } else {
        dispatch_sync(dispatch_get_main_queue(), publish);
    }
    return succeeded;
}
