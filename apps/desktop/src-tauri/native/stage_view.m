#import <AppKit/AppKit.h>

// The picture is a borderless child window above the webview. A view added
// inside the webview's parent is covered by WKWebView, so the screen would
// never appear. The child window is positioned from the stage rectangle and
// does not take clicks, which stay on the buttons in the webview.
void *dcmax_stage_place(void *ns_window, void *ns_content, void *existing, double x, double y, double w, double h, int visible) {
    @autoreleasepool {
        if (!ns_window || !ns_content) {
            return existing;
        }
        NSWindow *window = (__bridge NSWindow *)ns_window;
        NSView *content = (__bridge NSView *)ns_content;
        NSView *view = (__bridge NSView *)existing;
        NSWindow *child = view.window;
        if (!visible && !view) {
            return existing;
        }
        if (!view || child.parentWindow != window) {
            child = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 2, 2)
                                                 styleMask:NSWindowStyleMaskBorderless
                                                   backing:NSBackingStoreBuffered
                                                     defer:NO];
            child.releasedWhenClosed = NO;
            child.opaque = NO;
            child.backgroundColor = NSColor.clearColor;
            child.hasShadow = NO;
            child.ignoresMouseEvents = YES;
            child.collectionBehavior = NSWindowCollectionBehaviorFullScreenAuxiliary | NSWindowCollectionBehaviorIgnoresCycle;
            [window addChildWindow:child ordered:NSWindowAbove];
            view = child.contentView;
            view.wantsLayer = YES;
        }
        CGFloat height = NSHeight(content.bounds);
        NSRect local = NSMakeRect(x, height - y - h, MAX(w, 2.0), MAX(h, 2.0));
        NSRect inWindow = [content convertRect:local toView:nil];
        NSRect screen = [window convertRectToScreen:inWindow];
        [child setFrame:screen display:YES];
        if (view.layer) {
            view.layer.cornerRadius = 12.0;
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
