#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>

@interface NSPasteboard (Labrador)
- (NSArray *)getFilePaths;
@end

/// LabradorHostView is the content view of a Labrador window.
// It is backed by a Metal CALayer.
@interface LabradorHostView : NSView <CALayerDelegate, NSTextInputClient>
- (LabradorHostView *)initWithFrame:(NSRect)frame
                    metalDevice:(id)metalDevice
             enableTitlebarDrag:(BOOL)enableTitlebarDrag
                       testMode:(BOOL)testMode;
- (void)setAsyncCallback:(BOOL)shouldAsync;
- (BOOL)keyDownImpl:(NSEvent *)event;
@end
