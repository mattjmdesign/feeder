// Feeder's notifications, posted as Feeder (its name and icon) through UserNotifications.
// macOS asks once, "Feeder would like to send you notifications", the first time one is posted.
// Clicking a notification opens the feeder:// link it carries.

#import <AppKit/AppKit.h>
#import <UserNotifications/UserNotifications.h>

@interface FeederNotifier : NSObject <UNUserNotificationCenterDelegate>
@end

@implementation FeederNotifier

// Show banners while Feeder is open too; macOS hides them for the frontmost app by default.
- (void)userNotificationCenter:(UNUserNotificationCenter *)center
       willPresentNotification:(UNNotification *)notification
         withCompletionHandler:(void (^)(UNNotificationPresentationOptions))completionHandler {
    completionHandler(UNNotificationPresentationOptionBanner | UNNotificationPresentationOptionList);
}

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
    didReceiveNotificationResponse:(UNNotificationResponse *)response
             withCompletionHandler:(void (^)(void))completionHandler {
    NSString *link = response.notification.request.content.userInfo[@"url"];
    NSURL *url = link.length > 0 ? [NSURL URLWithString:link] : nil;
    if (url != nil && [[url.scheme lowercaseString] isEqualToString:@"feeder"]) {
        [NSWorkspace.sharedWorkspace openURL:url];
    }
    completionHandler();
}

@end

static FeederNotifier *notifier;

// UserNotifications needs an app bundle; `cargo run` has none.
static bool bundled(void) {
    return NSBundle.mainBundle.bundleIdentifier != nil;
}

// Installs the delegate at launch, so a click on a notification that outlived the last run
// still opens its link.
void feeder_notify_start(void) {
    if (!bundled() || notifier != nil) {
        return;
    }
    notifier = [FeederNotifier new];
    UNUserNotificationCenter.currentNotificationCenter.delegate = notifier;
}

// Posts a notification; `url` may be NULL. False outside an app bundle, where the caller
// falls back.
bool feeder_notify(const char *title, const char *body, const char *url) {
    if (!bundled()) {
        return false;
    }
    feeder_notify_start();
    NSString *titleText = [NSString stringWithUTF8String:title] ?: @"Feeder";
    NSString *bodyText = [NSString stringWithUTF8String:body] ?: @"";
    NSString *link = url != NULL ? [NSString stringWithUTF8String:url] : nil;
    UNUserNotificationCenter *center = UNUserNotificationCenter.currentNotificationCenter;
    [center requestAuthorizationWithOptions:UNAuthorizationOptionAlert
                          completionHandler:^(BOOL granted, __unused NSError *error) {
        if (!granted) {
            return;
        }
        UNMutableNotificationContent *content = [UNMutableNotificationContent new];
        content.title = titleText;
        content.body = bodyText;
        if (link != nil) {
            content.userInfo = @{@"url": link};
        }
        UNNotificationRequest *request = [UNNotificationRequest requestWithIdentifier:NSUUID.UUID.UUIDString
                                                                              content:content
                                                                              trigger:nil];
        [center addNotificationRequest:request withCompletionHandler:nil];
    }];
    return true;
}
