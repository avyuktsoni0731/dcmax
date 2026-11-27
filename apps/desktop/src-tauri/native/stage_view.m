#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>

// Owned for the life of the process. The Rust side used to keep the raw
// content view and pass it back in. After a share stopped, that pointer was
// already dead and the next hide crashed in objc_retain. Hide only orders
// this window out. It is not released.
static NSWindow *stageWindow = nil;

static NSWindow *stage_window(NSWindow *owner) {
    if (stageWindow && stageWindow.parentWindow != owner) {
        [stageWindow.parentWindow removeChildWindow:stageWindow];
        [owner addChildWindow:stageWindow ordered:NSWindowAbove];
    }
    if (!stageWindow) {
        stageWindow = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 2, 2)
                                                   styleMask:NSWindowStyleMaskBorderless
                                                     backing:NSBackingStoreBuffered
                                                       defer:NO];
        stageWindow.releasedWhenClosed = NO;
        stageWindow.opaque = NO;
        stageWindow.backgroundColor = NSColor.clearColor;
        stageWindow.hasShadow = NO;
        stageWindow.ignoresMouseEvents = YES;
        stageWindow.collectionBehavior = NSWindowCollectionBehaviorFullScreenAuxiliary | NSWindowCollectionBehaviorIgnoresCycle;
        stageWindow.contentView.wantsLayer = YES;
        [owner addChildWindow:stageWindow ordered:NSWindowAbove];
    }
    return stageWindow;
}

void *dcmax_stage_place(void *ns_window, void *ns_content, void *existing, double x, double y, double w, double h, int visible) {
    (void)existing;
    @autoreleasepool {
        if (!ns_window || !ns_content) {
            if (stageWindow) {
                [stageWindow orderOut:nil];
            }
            return stageWindow ? (__bridge void *)stageWindow.contentView : NULL;
        }
        NSWindow *owner = (__bridge NSWindow *)ns_window;
        NSView *content = (__bridge NSView *)ns_content;
        NSWindow *child = stage_window(owner);
        NSView *view = child.contentView;
        CGFloat height = NSHeight(content.bounds);
        NSRect local = NSMakeRect(x, height - y - h, MAX(w, 2.0), MAX(h, 2.0));
        NSRect inWindow = [content convertRect:local toView:nil];
        NSRect screen = [owner convertRectToScreen:inWindow];
        [child setFrame:screen display:YES];
        if (view.layer) {
            BOOL fills = w >= NSWidth(content.bounds) - 1.0 && h >= NSHeight(content.bounds) - 1.0;
            view.layer.cornerRadius = fills ? 0.0 : 12.0;
            view.layer.masksToBounds = YES;
        }
        if (visible) {
            if (!child.isVisible) {
                [child orderFront:nil];
            }
        } else if (child.isVisible) {
            [child orderOut:nil];
        }
        return (__bridge void *)view;
    }
}

void dcmax_stage_holes(void *ns_view, const double *xywh, int count, double radius) {
    @autoreleasepool {
        if (!ns_view) {
            return;
        }
        NSView *view = (__bridge NSView *)ns_view;
        CALayer *layer = view.layer;
        if (!layer) {
            return;
        }
        CGFloat width = NSWidth(view.bounds);
        CGFloat height = NSHeight(view.bounds);
        if (width < 2.0 || height < 2.0) {
            return;
        }
        CGMutablePathRef path = CGPathCreateMutable();
        CGRect bounds = CGRectMake(0.0, 0.0, width, height);
        if (radius > 0.5) {
            CGPathAddRoundedRect(path, nil, bounds, radius, radius);
        } else {
            CGPathAddRect(path, nil, bounds);
        }
        for (int index = 0; index < count; index++) {
            double x = xywh[index * 4];
            double y = xywh[index * 4 + 1];
            double holeWidth = xywh[index * 4 + 2];
            double holeHeight = xywh[index * 4 + 3];
            if (holeWidth < 1.0 || holeHeight < 1.0) {
                continue;
            }
            CGRect hole = CGRectMake(x, height - y - holeHeight, holeWidth, holeHeight);
            CGPathAddRoundedRect(path, nil, hole, 12.0, 12.0);
        }
        CAShapeLayer *mask = [CAShapeLayer layer];
        mask.frame = bounds;
        mask.fillRule = kCAFillRuleEvenOdd;
        mask.path = path;
        layer.mask = mask;
        CGPathRelease(path);
    }
}
