#import <AppKit/AppKit.h>

void *dcmax_stage_place(void *ns_window, void *existing, double x, double y, double w, double h, int visible) {
    @autoreleasepool {
        if (!ns_window) {
            return existing;
        }
        NSWindow *window = (__bridge NSWindow *)ns_window;
        NSView *content = window.contentView;
        if (!content) {
            return existing;
        }
        NSView *view = (__bridge NSView *)existing;
        if (!view) {
            view = [[NSView alloc] initWithFrame:NSZeroRect];
            view.wantsLayer = YES;
            [content addSubview:view positioned:NSWindowAbove relativeTo:nil];
        }
        CGFloat height = NSHeight(content.bounds);
        view.frame = NSMakeRect(x, height - y - h, MAX(w, 2), MAX(h, 2));
        view.hidden = visible ? NO : YES;
        return (__bridge void *)view;
    }
}
