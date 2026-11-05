// A virtual display for frame measurements (scripts/bench.sh): the front ends draw at the
// refresh rate of the display their window is on, so a window parked here runs at a known rate
// without taking the user's screen. Uses CoreGraphics' private CGVirtualDisplay classes.
// Usage: vdisplay WIDTH HEIGHT HZ — prints "display ID X Y WIDTH HEIGHT HZ" (points, global
// coordinates, the mode's refresh rate) once the display is up; it exists while this runs.
// Build: clang -fobjc-arc -framework Foundation -framework CoreGraphics -o vdisplay vdisplay.m
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#include <unistd.h>

@interface CGVirtualDisplayDescriptor : NSObject
@property(retain) dispatch_queue_t queue;
@property(copy) NSString *name;
@property unsigned int maxPixelsWide;
@property unsigned int maxPixelsHigh;
@property CGSize sizeInMillimeters;
@property unsigned int vendorID;
@property unsigned int productID;
@property unsigned int serialNum;
@property(copy) void (^terminationHandler)(id, id);
@end

@interface CGVirtualDisplayMode : NSObject
- (instancetype)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)rate;
@end

@interface CGVirtualDisplaySettings : NSObject
@property unsigned int hiDPI;
@property(retain) NSArray *modes;
@end

@interface CGVirtualDisplay : NSObject
- (instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@property(readonly) CGDirectDisplayID displayID;
@end

static const unsigned int vendor = 0x4d53, product = 0x0035;

// The display by its identity: `displayID` can read 0 although the display came up.
static CGDirectDisplayID findDisplay(unsigned int serial) {
    CGDirectDisplayID ids[32];
    uint32_t count = 0;
    CGGetOnlineDisplayList(32, ids, &count);
    for (uint32_t i = 0; i < count; i++) {
        if (CGDisplayVendorNumber(ids[i]) == vendor && CGDisplayModelNumber(ids[i]) == product &&
            CGDisplaySerialNumber(ids[i]) == serial)
            return ids[i];
    }
    return 0;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc != 4) {
            fprintf(stderr, "usage: vdisplay WIDTH HEIGHT HZ\n");
            return 2;
        }
        unsigned int w = atoi(argv[1]), h = atoi(argv[2]);
        double hz = atof(argv[3]);
        // Unique per process, so several measurements can each have their own display.
        unsigned int serial = getpid();
        CGVirtualDisplayDescriptor *d = [CGVirtualDisplayDescriptor new];
        d.queue = dispatch_get_main_queue();
        d.name = @"Mailstream Bench";
        d.maxPixelsWide = w * 2;
        d.maxPixelsHigh = h * 2;
        d.sizeInMillimeters = CGSizeMake(w / 4.0, h / 4.0);
        d.vendorID = vendor;
        d.productID = product;
        d.serialNum = serial;
        d.terminationHandler = ^(id a, id b) { exit(0); };
        static CGVirtualDisplay *display;
        display = [[CGVirtualDisplay alloc] initWithDescriptor:d];
        CGVirtualDisplaySettings *s = [CGVirtualDisplaySettings new];
        s.hiDPI = 1;
        s.modes = @[ [[CGVirtualDisplayMode alloc] initWithWidth:w height:h refreshRate:hz] ];
        if (![display applySettings:s]) {
            fprintf(stderr, "vdisplay: the display refused its settings\n");
            return 1;
        }
        // The display comes up asynchronously: wait up to 5 s for it to have its mode.
        static int tries = 0;
        static void (^poll)(void);
        poll = ^{
            CGDirectDisplayID id = display.displayID ?: findDisplay(serial);
            CGRect b = id ? CGDisplayBounds(id) : CGRectZero;
            if (id && b.size.width > 0) {
                CGDisplayModeRef mode = CGDisplayCopyDisplayMode(id);
                double rate = mode ? CGDisplayModeGetRefreshRate(mode) : 0;
                if (mode) CGDisplayModeRelease(mode);
                printf("display %u %.0f %.0f %.0f %.0f %.0f\n", id, b.origin.x, b.origin.y, b.size.width,
                       b.size.height, rate);
                fflush(stdout);
            } else if (++tries < 50) {
                dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC), dispatch_get_main_queue(), poll);
            } else {
                fprintf(stderr, "vdisplay: the display did not come up\n");
                exit(1);
            }
        };
        dispatch_async(dispatch_get_main_queue(), poll);
        dispatch_main();
    }
}
